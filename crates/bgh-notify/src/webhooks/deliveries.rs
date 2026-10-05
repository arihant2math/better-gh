//! Webhook delivery log: list (cursor pagination), get, redeliver.

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Need, Owner, load_hook, org_owner, repo_owner};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeliveryRow {
    pub id: i64,
    pub hook_id: i64,
    pub guid: uuid::Uuid,
    pub event: String,
    pub action: Option<String>,
    pub repo_id: Option<i64>,
    pub installation_id: Option<i64>,
    pub redelivery: bool,
    pub status: String,
    pub status_code: Option<i32>,
    pub duration_ms: Option<i32>,
    pub request_headers: Value,
    pub response_headers: Option<Value>,
    pub response_body: Option<String>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub url: String,
    pub payload_raw: String,
    pub content_type: String,
    pub attempts: i32,
    pub throttled_at: Option<DateTime<Utc>>,
}

impl DeliveryRow {
    pub const COLUMNS: &'static str = "id, hook_id, guid, event, action, repo_id, \
        installation_id, redelivery, status, status_code, duration_ms, request_headers, \
        response_headers, response_body, delivered_at, created_at, url, payload_raw, \
        content_type, attempts, throttled_at";
}

/// GitHub `hook-delivery-item`.
#[derive(Debug, Clone, Serialize)]
pub struct DeliveryItem {
    pub id: i64,
    pub guid: String,
    pub delivered_at: Timestamp,
    pub redelivery: bool,
    pub duration: f64,
    pub status: String,
    pub status_code: i32,
    pub event: String,
    pub action: Option<String>,
    pub installation_id: Option<i64>,
    pub repository_id: Option<i64>,
    pub throttled_at: Option<Timestamp>,
}

/// GitHub `hook-delivery`.
#[derive(Debug, Clone, Serialize)]
pub struct Delivery {
    #[serde(flatten)]
    pub item: DeliveryItem,
    pub url: String,
    pub request: Value,
    pub response: Value,
}

fn item(d: &DeliveryRow) -> DeliveryItem {
    DeliveryItem {
        id: d.id,
        guid: d.guid.to_string(),
        delivered_at: d.delivered_at.unwrap_or(d.created_at).into(),
        redelivery: d.redelivery,
        duration: f64::from(d.duration_ms.unwrap_or(0)) / 1000.0,
        status: d.status.clone(),
        status_code: d.status_code.unwrap_or(0),
        event: d.event.clone(),
        action: d.action.clone(),
        installation_id: d.installation_id,
        repository_id: d.repo_id,
        throttled_at: ts(d.throttled_at),
    }
}

fn detail(d: &DeliveryRow) -> Delivery {
    let payload: Value = serde_json::from_str(&d.payload_raw).unwrap_or(Value::Null);
    Delivery {
        item: item(d),
        url: d.url.clone(),
        request: json!({
            "headers": d.request_headers,
            "payload": payload,
        }),
        response: json!({
            "headers": d.response_headers.clone().unwrap_or_else(|| json!({})),
            "payload": d.response_body,
        }),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ListParams {
    pub per_page: Option<u32>,
    pub cursor: Option<String>,
    /// `success` | `failure`
    pub status: Option<String>,
    pub redelivery: Option<bool>,
}

fn parse_cursor(c: Option<&str>) -> ApiResult<Option<i64>> {
    match c.filter(|c| !c.is_empty()) {
        None => Ok(None),
        Some(c) => c
            .strip_prefix("v1_")
            .unwrap_or(c)
            .parse::<i64>()
            .map(Some)
            .map_err(|_| ApiError::invalid_field(FieldError::invalid("HookDelivery", "cursor"))),
    }
}

async fn list(state: &AppState, owner: &Owner, hook_id: i64, q: ListParams) -> ApiResult<Response> {
    let hook = load_hook(state, owner, hook_id).await?;
    let per_page = i64::from(q.per_page.unwrap_or(30).clamp(1, 100));
    let before = parse_cursor(q.cursor.as_deref())?;
    let status = match q.status.as_deref() {
        None | Some("") => None,
        Some("success") => Some(true),
        Some("failure") => Some(false),
        Some(_) => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "HookDelivery",
                "status",
            )));
        }
    };
    let mut rows: Vec<DeliveryRow> = sqlx::query_as(&format!(
        "SELECT {} FROM webhook_deliveries
          WHERE hook_id = $1 AND ($2::bigint IS NULL OR id < $2)
            AND ($3::boolean IS NULL OR (status = 'OK') = $3)
            AND ($4::boolean IS NULL OR redelivery = $4)
          ORDER BY id DESC LIMIT $5",
        DeliveryRow::COLUMNS
    ))
    .bind(hook.id)
    .bind(before)
    .bind(status)
    .bind(q.redelivery)
    .bind(per_page + 1)
    .fetch_all(&state.db)
    .await?;
    let has_next = rows.len() as i64 > per_page;
    rows.truncate(per_page as usize);
    let items: Vec<DeliveryItem> = rows.iter().map(item).collect();
    let mut resp = Json(items).into_response();
    if has_next && let Some(last) = rows.last() {
        let base = format!("{}/{}/deliveries", owner.hooks_url(state), hook.id);
        let mut extra = String::new();
        if let Some(s) = &q.status {
            extra.push_str(&format!("&status={s}"));
        }
        let link = format!(
            "<{base}?per_page={per_page}&cursor=v1_{}{extra}>; rel=\"next\"",
            last.id
        );
        if let Ok(v) = HeaderValue::from_str(&link) {
            resp.headers_mut().insert(header::LINK, v);
        }
    }
    Ok(resp)
}

async fn load(state: &AppState, owner: &Owner, hook_id: i64, id: i64) -> ApiResult<DeliveryRow> {
    let hook = load_hook(state, owner, hook_id).await?;
    sqlx::query_as(&format!(
        "SELECT {} FROM webhook_deliveries WHERE id = $1 AND hook_id = $2",
        DeliveryRow::COLUMNS
    ))
    .bind(id)
    .bind(hook.id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn redeliver(
    state: &AppState,
    owner: &Owner,
    hook_id: i64,
    id: i64,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let d = load(state, owner, hook_id, id).await?;
    let hook = load_hook(state, owner, hook_id).await?;
    let mut tx = Tx::begin(state).await?;
    let new_id: i64 = sqlx::query_scalar(
        "INSERT INTO webhook_deliveries
                (hook_id, guid, event, action, repo_id, installation_id, redelivery, status,
                 url, payload_raw, content_type)
         VALUES ($1, $2, $3, $4, $5, $6, true, 'pending', $7, $8, $9) RETURNING id",
    )
    .bind(hook.id)
    .bind(d.guid)
    .bind(&d.event)
    .bind(&d.action)
    .bind(d.repo_id)
    .bind(d.installation_id)
    .bind(&hook.url)
    .bind(&d.payload_raw)
    .bind(&hook.content_type)
    .fetch_one(&mut *tx)
    .await?;
    tx.enqueue(&super::deliver::DeliverWebhook {
        delivery_id: new_id,
    })
    .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

type RepoHookPath = Path<(String, String, i64)>;
type RepoDeliveryPath = Path<(String, String, i64, i64)>;

/// `GET /repos/{owner}/{repo}/hooks/{hook_id}/deliveries`
pub async fn repo_list(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r, hook)): RepoHookPath,
    Query(q): Query<ListParams>,
) -> ApiResult<Response> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Read).await?;
    list(&state, &owner, hook, q).await
}

/// `GET /repos/{owner}/{repo}/hooks/{hook_id}/deliveries/{delivery_id}`
pub async fn repo_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r, hook, id)): RepoDeliveryPath,
) -> ApiResult<Json<Delivery>> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Read).await?;
    Ok(Json(detail(&load(&state, &owner, hook, id).await?)))
}

/// `POST /repos/{owner}/{repo}/hooks/{hook_id}/deliveries/{delivery_id}/attempts`
pub async fn repo_redeliver(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((o, r, hook, id)): RepoDeliveryPath,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let owner = repo_owner(&state, &auth, &o, &r, Need::Write).await?;
    redeliver(&state, &owner, hook, id).await
}

type OrgHookPath = Path<(String, i64)>;
type OrgDeliveryPath = Path<(String, i64, i64)>;

/// `GET /orgs/{org}/hooks/{hook_id}/deliveries`
pub async fn org_list(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, hook)): OrgHookPath,
    Query(q): Query<ListParams>,
) -> ApiResult<Response> {
    let owner = org_owner(&state, &auth, &org).await?;
    list(&state, &owner, hook, q).await
}

/// `GET /orgs/{org}/hooks/{hook_id}/deliveries/{delivery_id}`
pub async fn org_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, hook, id)): OrgDeliveryPath,
) -> ApiResult<Json<Delivery>> {
    let owner = org_owner(&state, &auth, &org).await?;
    Ok(Json(detail(&load(&state, &owner, hook, id).await?)))
}

/// `POST /orgs/{org}/hooks/{hook_id}/deliveries/{delivery_id}/attempts`
pub async fn org_redeliver(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, hook, id)): OrgDeliveryPath,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let owner = org_owner(&state, &auth, &org).await?;
    redeliver(&state, &owner, hook, id).await
}
