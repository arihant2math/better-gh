//! REST endpoints under `/repos/{owner}/{repo}/actions/*`,
//! `/orgs/{org}/actions/*` and `/repos/{owner}/{repo}/environments/*`.

pub mod access;
pub mod artifacts;
pub mod deployments;
pub mod environments;
pub mod runners;
pub mod runs;
pub mod secrets;
pub mod variables;
pub mod workflows;

use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use serde::Serialize;
use serde_json::{Map, Value};

/// GitHub's wrapped list body (`{"total_count": n, "<key>": [...]}`) with
/// a `Link` header.
pub fn wrapped<T: Serialize>(p: &Pagination, total: i64, key: &str, items: Vec<T>) -> Response {
    let has_next = p.offset() + (items.len() as i64) < total;
    let mut body = Map::new();
    body.insert("total_count".into(), Value::from(total));
    body.insert(
        key.into(),
        serde_json::to_value(items).unwrap_or(Value::Array(vec![])),
    );
    let mut resp = Json(Value::Object(body)).into_response();
    if let Some(link) = p.link_header(has_next, Some(total))
        && let Ok(v) = HeaderValue::from_str(&link)
    {
        resp.headers_mut().insert(header::LINK, v);
    }
    resp
}

/// Load an organization by login (404 unless it is one).
pub async fn load_org(state: &AppState, org: &str) -> ApiResult<db::User> {
    db::User::find_by_login(&state.db, org)
        .await?
        .filter(|u| u.is_org())
        .ok_or(ApiError::NotFound)
}

/// Require organization admin (404 for non-members, 403 for members).
pub async fn require_org_admin(
    state: &AppState,
    auth: &AuthContext,
    org: &db::User,
) -> ApiResult<()> {
    if auth.user.site_admin {
        return Ok(());
    }
    match bgh_core::perms::org_role(&state.db, org.id, auth.user.id).await? {
        Some(r) if r == "admin" => {
            auth.require_scope("admin:org")?;
            Ok(())
        }
        Some(_) => Err(ApiError::forbidden("Must be an organization owner.")),
        None => Err(ApiError::NotFound),
    }
}

/// Valid secret / variable name per GitHub rules.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.to_ascii_uppercase().starts_with("GITHUB_")
        && !name.as_bytes()[0].is_ascii_digit()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
