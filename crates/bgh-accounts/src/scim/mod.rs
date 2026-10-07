//! SCIM 2.0 provisioning (RFC 7643/7644, GitHub's shapes).
//!
//! Enabled by the `auth_providers.scim` site setting. Endpoints (under
//! `/api/v3`):
//!
//! | Path | Who |
//! |---|---|
//! | `/scim/v2/enterprises/{enterprise}/Users[/{id}]` | site admins, token scope `scim:enterprise` (the slug is not checked: one enterprise per instance) |
//! | `/scim/v2/enterprises/{enterprise}/Groups[/{id}]` | same |
//! | `/scim/v2/organizations/{org}/Users[/{id}]` | organization owners, `admin:org` |
//!
//! Enterprise users are instance accounts: create links the account with
//! the same login (else creates one), `active: false` and `DELETE`
//! deprovision it — suspended, sessions ended, personal access tokens,
//! OAuth tokens and SSH keys revoked ([`deprovision`]) — and
//! `active: true` lifts a suspension SCIM made. Roles
//! `enterprise_owner` grant site admin. Groups drive the members of teams
//! mapped to them (`external_group_mappings` provider `scim`, matched by
//! display name, id or externalId; GitHub's team-sync REST maps all IdP
//! providers at once).
//!
//! Organization users are memberships: create adds the matching account
//! to the organization (creating the account only when sign-up is open or
//! the caller is a site admin), `active: false` / `DELETE` remove it.
//!
//! Lists take `filter` (`attr eq "value"` clauses joined by `and`),
//! `startIndex` (1-based) and `count`; `PATCH` takes `PatchOp` operations
//! (`add` / `replace` / `remove`, with or without `path`, Azure AD's
//! capitalized ops and string booleans included). Errors use the SCIM
//! error schema plus GitHub's `message` / `documentation_url`.

pub mod groups;
pub mod users;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::audit;
use bgh_core::prelude::*;
use bgh_core::settings;
use serde::Deserialize;
use serde_json::{Value, json};

pub const USER_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
pub const GROUP_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";
pub const LIST_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:ListResponse";
pub const ERROR_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:Error";
pub const PATCH_SCHEMA: &str = "urn:ietf:params:scim:api:messages:2.0:PatchOp";
/// `external_group_mappings.provider` of SCIM groups.
pub const PROVIDER: &str = "scim";
const DOCS: &str = "https://docs.github.com/enterprise-server/rest/enterprise-admin/scim";
const SCIM_JSON: &str = "application/scim+json; charset=utf-8";

/// A SCIM error response.
#[derive(Debug)]
pub struct ScimError {
    pub status: StatusCode,
    pub scim_type: Option<&'static str>,
    pub detail: String,
}

impl ScimError {
    pub fn new(
        status: StatusCode,
        scim_type: Option<&'static str>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            status,
            scim_type,
            detail: detail.into(),
        }
    }

    pub fn bad(scim_type: &'static str, detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, Some(scim_type), detail)
    }

    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, Some("uniqueness"), detail)
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, None, "Resource not found")
    }
}

impl From<ApiError> for ScimError {
    fn from(e: ApiError) -> Self {
        let status = e.status();
        let detail = match &e {
            ApiError::Internal(_) => {
                // Logged by the ApiError renderer.
                let _ = e.into_response();
                "Server Error".to_string()
            }
            ApiError::NotFound => "Resource not found".to_string(),
            other => other.to_string(),
        };
        Self::new(status, None, detail)
    }
}

impl From<sqlx::Error> for ScimError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::from(e).into()
    }
}

impl IntoResponse for ScimError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "schemas": [ERROR_SCHEMA],
            "message": self.detail,
            "detail": self.detail,
            "status": self.status.as_u16(),
            "documentation_url": DOCS,
        });
        if let Some(t) = self.scim_type {
            body["scimType"] = json!(t);
        }
        scim_json(self.status, body)
    }
}

pub type ScimResult<T> = Result<T, ScimError>;

/// A SCIM JSON response (`application/scim+json`).
pub fn scim_json(status: StatusCode, body: Value) -> Response {
    let mut resp = (status, axum::Json(body)).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(SCIM_JSON));
    resp
}

/// Who a request provisions for.
#[derive(Debug, Clone)]
pub enum Tenant {
    Enterprise,
    Org(Box<db::User>),
}

impl Tenant {
    pub fn org_id(&self) -> Option<i64> {
        match self {
            Tenant::Enterprise => None,
            Tenant::Org(o) => Some(o.id),
        }
    }

    pub fn base(&self, state: &AppState, enterprise: &str) -> String {
        match self {
            Tenant::Enterprise => state
                .urls
                .api(&format!("/scim/v2/enterprises/{enterprise}")),
            Tenant::Org(o) => state
                .urls
                .api(&format!("/scim/v2/organizations/{}", o.login)),
        }
    }
}

async fn enabled(state: &AppState) -> ScimResult<()> {
    let s = settings::load(state).await?;
    if s.auth_providers.scim.enabled {
        Ok(())
    } else {
        Err(ScimError::not_found())
    }
}

/// Enterprise endpoints: site admins with `scim:enterprise`.
pub async fn enterprise_auth(state: &AppState, auth: &MaybeUser) -> ScimResult<AuthContext> {
    enabled(state).await?;
    let a = auth.as_ref().ok_or_else(ApiError::requires_auth)?;
    if !a.user.site_admin {
        return Err(ScimError::new(
            StatusCode::FORBIDDEN,
            None,
            "Must be an enterprise owner (site administrator).",
        ));
    }
    a.require_scope("scim:enterprise")?;
    Ok(a.clone())
}

/// Organization endpoints: org owners (or site admins) with `admin:org`.
pub async fn org_auth(
    state: &AppState,
    auth: &MaybeUser,
    org: &str,
) -> ScimResult<(AuthContext, db::User)> {
    enabled(state).await?;
    let a = auth.as_ref().ok_or_else(ApiError::requires_auth)?;
    let org = db::User::find_by_login(&state.db, org)
        .await?
        .filter(|o| o.is_org())
        .ok_or_else(ScimError::not_found)?;
    let role = bgh_core::perms::org_role(&state.db, org.id, a.user.id).await?;
    if !role.is_some_and(|r| r.is_admin()) && !a.user.site_admin {
        return Err(if role.is_some() {
            ScimError::new(
                StatusCode::FORBIDDEN,
                None,
                "You must be an organization owner to do that.",
            )
        } else {
            ScimError::not_found()
        });
    }
    a.require_scope("admin:org")?;
    Ok((a.clone(), org))
}

/// `startIndex` / `count` / `filter` query parameters.
#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    #[serde(rename = "startIndex")]
    pub start_index: Option<i64>,
    pub count: Option<i64>,
    pub filter: Option<String>,
}

impl ListQuery {
    pub fn offset(&self) -> i64 {
        self.start_index.unwrap_or(1).max(1) - 1
    }

    pub fn limit(&self) -> i64 {
        self.count.unwrap_or(100).clamp(0, 1000)
    }

    pub fn filter(&self) -> ScimResult<Vec<(String, String)>> {
        match self
            .filter
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
        {
            Some(f) => parse_filter(f),
            None => Ok(Vec::new()),
        }
    }
}

/// A list response.
pub fn list_response(total: i64, start: i64, items: Vec<Value>) -> Response {
    scim_json(
        StatusCode::OK,
        json!({
            "schemas": [LIST_SCHEMA],
            "totalResults": total,
            "itemsPerPage": items.len(),
            "startIndex": start + 1,
            "Resources": items,
        }),
    )
}

/// Parse `attr eq "value" [and attr eq "value"]…` into lower-cased
/// attribute names and values.
pub fn parse_filter(f: &str) -> ScimResult<Vec<(String, String)>> {
    let bad = || ScimError::bad("invalidFilter", format!("Unsupported filter: {f}"));
    let mut out = Vec::new();
    let mut rest = f.trim();
    loop {
        let (attr, after) = rest.split_once(char::is_whitespace).ok_or_else(bad)?;
        let after = after.trim_start();
        let (op, after) = after.split_once(char::is_whitespace).ok_or_else(bad)?;
        if !op.eq_ignore_ascii_case("eq") {
            return Err(bad());
        }
        let after = after.trim_start();
        let mut chars = after.char_indices();
        if chars.next().map(|(_, c)| c) != Some('"') {
            return Err(bad());
        }
        let mut value = String::new();
        let mut end = None;
        let mut escaped = false;
        for (i, c) in chars {
            if escaped {
                value.push(c);
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                end = Some(i);
                break;
            } else {
                value.push(c);
            }
        }
        let end = end.ok_or_else(bad)?;
        out.push((attr.to_ascii_lowercase(), value));
        rest = after[end + 1..].trim_start();
        if rest.is_empty() {
            return Ok(out);
        }
        let (and, next) = rest.split_once(char::is_whitespace).ok_or_else(bad)?;
        if !and.eq_ignore_ascii_case("and") {
            return Err(bad());
        }
        rest = next.trim_start();
    }
}

/// One `PatchOp` operation, normalized.
#[derive(Debug, Clone)]
pub struct PatchOperation {
    /// `add` | `replace` | `remove`
    pub op: String,
    pub path: Option<String>,
    pub value: Value,
}

/// Parse a `PatchOp` body.
pub fn patch_operations(body: &Value) -> ScimResult<Vec<PatchOperation>> {
    let ops = body
        .get("Operations")
        .or_else(|| body.get("operations"))
        .and_then(Value::as_array)
        .ok_or_else(|| ScimError::bad("invalidSyntax", "Operations is required"))?;
    ops.iter()
        .map(|o| {
            let op = o
                .get("op")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
                .filter(|op| matches!(op.as_str(), "add" | "replace" | "remove"))
                .ok_or_else(|| {
                    ScimError::bad("invalidSyntax", "op must be add, replace or remove")
                })?;
            Ok(PatchOperation {
                op,
                path: o
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_string),
                value: o.get("value").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

/// A SCIM boolean (Azure AD sends `"True"` / `"False"`).
pub fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
        Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// Suspend an account on behalf of the IdP and revoke its credentials:
/// sessions, personal access tokens, OAuth tokens and SSH keys. The last
/// active site administrator is left alone (logged). Returns whether the
/// account was suspended by this call.
pub async fn deprovision(state: &AppState, user: &db::User, reason: &str) -> ApiResult<bool> {
    let mut tx = Tx::begin(state).await?;
    if user.site_admin {
        let others: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM users WHERE site_admin AND type = 'User' AND id <> $1
               AND suspended_at IS NULL",
        )
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
        if others == 0 {
            tracing::warn!(user = %user.login, "SCIM: not deprovisioning the last site administrator");
            return Ok(false);
        }
    }
    let suspended = sqlx::query(
        "UPDATE users SET suspended_at = now(), suspended_reason = $2, site_admin = false,
                          updated_at = now()
          WHERE id = $1 AND suspended_at IS NULL",
    )
    .bind(user.id)
    .bind(format!("SCIM: {reason}"))
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    let tokens =
        sqlx::query("DELETE FROM access_tokens WHERE user_id = $1 AND kind IN ('pat', 'oauth')")
            .bind(user.id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    let keys = sqlx::query("DELETE FROM ssh_keys WHERE user_id = $1")
        .bind(user.id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    audit::log(
        &mut *tx,
        None,
        "user.suspend",
        audit::Target::User(user.id),
        json!({
            "login": user.login,
            "reason": format!("SCIM: {reason}"),
            "scim": true,
            "revoked_tokens": tokens,
            "revoked_ssh_keys": keys,
        }),
    )
    .await?;
    if suspended {
        tx.emit(Event::UserAccountChanged {
            user_id: user.id,
            login: user.login.clone(),
            action: "suspended".into(),
            actor_id: user.id,
            data: json!({ "scim": true }),
        });
        tx.sync_user(user.id).await?;
    }
    tx.commit().await?;
    bgh_core::auth::destroy_user_sessions(state, user.id).await?;
    Ok(suspended)
}

/// Lift a suspension made by SCIM.
pub async fn reactivate(state: &AppState, user: &db::User) -> ApiResult<()> {
    let mut tx = Tx::begin(state).await?;
    let lifted = sqlx::query(
        "UPDATE users SET suspended_at = NULL, suspended_reason = NULL, updated_at = now()
          WHERE id = $1 AND suspended_at IS NOT NULL",
    )
    .bind(user.id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if lifted {
        audit::log(
            &mut *tx,
            None,
            "user.unsuspend",
            audit::Target::User(user.id),
            json!({ "login": user.login, "reason": "SCIM: reactivated", "scim": true }),
        )
        .await?;
        tx.emit(Event::UserAccountChanged {
            user_id: user.id,
            login: user.login.clone(),
            action: "unsuspended".into(),
            actor_id: user.id,
            data: json!({ "scim": true }),
        });
        tx.sync_user(user.id).await?;
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters() {
        assert_eq!(
            parse_filter(r#"userName eq "mona@example.com""#).unwrap(),
            vec![("username".into(), "mona@example.com".into())]
        );
        assert_eq!(
            parse_filter(r#"externalId Eq "a\"b" and userName eq "x""#).unwrap(),
            vec![
                ("externalid".into(), "a\"b".into()),
                ("username".into(), "x".into())
            ]
        );
        assert!(parse_filter(r#"userName co "x""#).is_err());
        assert!(parse_filter(r#"userName eq x"#).is_err());
        assert!(parse_filter(r#"userName eq "x" or id eq "y""#).is_err());
    }

    #[test]
    fn patch_ops() {
        let ops = patch_operations(&json!({
            "schemas": [PATCH_SCHEMA],
            "Operations": [{"op": "Replace", "path": "active", "value": "False"}]
        }))
        .unwrap();
        assert_eq!(ops[0].op, "replace");
        assert_eq!(as_bool(&ops[0].value), Some(false));
        assert!(patch_operations(&json!({"Operations": [{"op": "move"}]})).is_err());
    }
}
