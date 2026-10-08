//! Audit log search: `GET /_bgh/admin/audit-log` (admin UI),
//! `GET /orgs/{org}/audit-log` and `GET /enterprises/{enterprise}/audit-log`
//! (GitHub shapes). Cursor pagination on the entry id.
//!
//! Filters come from explicit parameters and/or a GitHub search `phrase`:
//! `action:repo.create`, `action:repo` (category), `actor:alice`,
//! `user:bob`, `org:acme`, `repo:acme/web`, `created:>=2024-01-01`,
//! `created:2024-01-01..2024-02-01`, `created:2024-03-05`.

use std::collections::HashMap;

use axum::extract::{OriginalUri, State};
use axum::http::{HeaderValue, Uri, header};
use axum::response::{IntoResponse, Response};
use bgh_core::perms;
use bgh_core::prelude::*;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::{FromRow, Postgres, QueryBuilder};

use crate::common::{self, like_escape};

/// Parsed search filters.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Filter {
    /// Exact actions (`repo.create`) or categories (`repo`).
    pub actions: Vec<String>,
    pub actors: Vec<String>,
    pub users: Vec<String>,
    pub orgs: Vec<String>,
    /// `owner/name`
    pub repos: Vec<String>,
    pub from: Option<DateTime<Utc>>,
    /// Exclusive upper bound.
    pub to: Option<DateTime<Utc>>,
}

fn day_start(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc())
}

/// Whether `s` is a bare date (whole day) rather than a timestamp.
fn is_day(s: &str) -> bool {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// Apply a `created:` qualifier. Returns false if it doesn't parse.
fn apply_created(f: &mut Filter, v: &str) -> bool {
    let end_of = |s: &str| -> Option<DateTime<Utc>> {
        let t = day_start(s)?;
        Some(if is_day(s) { t + Duration::days(1) } else { t })
    };
    if let Some((a, b)) = v.split_once("..") {
        if a != "*" {
            match day_start(a) {
                Some(t) => f.from = Some(t),
                None => return false,
            }
        }
        if b != "*" {
            match end_of(b) {
                Some(t) => f.to = Some(t),
                None => return false,
            }
        }
        return true;
    }
    let (op, rest) = ["<=", ">=", "<", ">"]
        .iter()
        .find_map(|op| v.strip_prefix(op).map(|r| (*op, r)))
        .unwrap_or(("=", v));
    let parsed = match op {
        ">=" => day_start(rest).map(|t| f.from = Some(t)),
        ">" => end_of(rest).map(|t| f.from = Some(t)),
        "<" => day_start(rest).map(|t| f.to = Some(t)),
        "<=" => end_of(rest).map(|t| f.to = Some(t)),
        _ => day_start(rest).and_then(|t| {
            f.from = Some(t);
            end_of(rest).map(|e| f.to = Some(e))
        }),
    };
    parsed.is_some()
}

impl Filter {
    /// Parse a GitHub audit log search phrase. Unknown qualifiers → 422.
    pub fn parse_phrase(&mut self, phrase: &str) -> ApiResult<()> {
        for token in phrase.split_whitespace() {
            let Some((k, v)) = token.split_once(':') else {
                // A bare word searches action names.
                self.actions.push(token.to_string());
                continue;
            };
            let v = v.trim_matches('"').to_string();
            if v.is_empty() {
                continue;
            }
            match k {
                "action" | "operation" => self.actions.push(v),
                "actor" => self.actors.push(v),
                "user" => self.users.push(v),
                "org" => self.orgs.push(v),
                "repo" => self.repos.push(v),
                "created" => {
                    if !apply_created(self, &v) {
                        return Err(ApiError::unprocessable(format!(
                            "Invalid created qualifier {v:?}"
                        )));
                    }
                }
                _ => {
                    return Err(ApiError::unprocessable(format!(
                        "Unknown search qualifier {k:?}"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, FromRow)]
pub struct Row {
    pub id: i64,
    pub actor_id: Option<i64>,
    pub actor_login: Option<String>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<i64>,
    pub org_id: Option<i64>,
    pub repo_id: Option<i64>,
    pub data: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Resolve logins/full names to ids and run the query. `None` = a name
/// didn't resolve (no results).
async fn search(
    state: &AppState,
    f: &Filter,
    org_id: Option<i64>,
    cursor: Option<i64>,
    ascending: bool,
    limit: i64,
) -> ApiResult<Vec<Row>> {
    let mut q: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT id, actor_id, actor_login, action, target_type, target_id, org_id, repo_id,
                data, ip, created_at FROM audit_log WHERE true",
    );
    if let Some(org_id) = org_id {
        q.push(" AND org_id = ").push_bind(org_id);
    }
    if !f.actions.is_empty() {
        q.push(" AND (");
        for (i, a) in f.actions.iter().enumerate() {
            if i > 0 {
                q.push(" OR ");
            }
            if a.contains('.') {
                q.push("action = ").push_bind(a.clone());
            } else {
                q.push("action LIKE ")
                    .push_bind(format!("{}.%", like_escape(a)));
            }
        }
        q.push(")");
    }
    if !f.actors.is_empty() {
        let logins: Vec<String> = f.actors.iter().map(|a| a.to_lowercase()).collect();
        q.push(" AND lower(actor_login) = ANY(")
            .push_bind(logins)
            .push(")");
    }
    if !f.users.is_empty() {
        let logins: Vec<String> = f.users.iter().map(|a| a.to_lowercase()).collect();
        q.push(
            " AND target_type = 'user' AND target_id IN (SELECT id FROM users WHERE lower(login) = ANY(",
        )
        .push_bind(logins)
        .push("))");
    }
    if !f.orgs.is_empty() {
        let logins: Vec<String> = f.orgs.iter().map(|a| a.to_lowercase()).collect();
        q.push(" AND org_id IN (SELECT id FROM users WHERE type = 'Organization' AND lower(login) = ANY(")
            .push_bind(logins)
            .push("))");
    }
    if !f.repos.is_empty() {
        let mut ids = Vec::new();
        for full in &f.repos {
            if let Some((o, n)) = full.split_once('/')
                && let Ok((_, r)) = common::repo(state, o, n).await
            {
                ids.push(r.id);
            }
        }
        q.push(" AND repo_id = ANY(").push_bind(ids).push(")");
    }
    if let Some(t) = f.from {
        q.push(" AND created_at >= ").push_bind(t);
    }
    if let Some(t) = f.to {
        q.push(" AND created_at < ").push_bind(t);
    }
    if let Some(c) = cursor {
        q.push(if ascending {
            " AND id > "
        } else {
            " AND id < "
        })
        .push_bind(c);
    }
    q.push(if ascending {
        " ORDER BY id ASC LIMIT "
    } else {
        " ORDER BY id DESC LIMIT "
    })
    .push_bind(limit);
    Ok(q.build_query_as::<Row>().fetch_all(&state.db).await?)
}

/// Names for the ids referenced by `rows` (one query each).
struct Names {
    users: HashMap<i64, db::User>,
    repos: HashMap<i64, String>,
}

async fn names(state: &AppState, rows: &[Row]) -> ApiResult<Names> {
    let ids = rows.iter().flat_map(|r| {
        [
            r.org_id,
            r.actor_id,
            (r.target_type.as_deref() == Some("user"))
                .then_some(r.target_id)
                .flatten(),
        ]
    });
    let users = bgh_core::views::users_by_id(state, ids).await?;
    let mut repo_ids: Vec<i64> = rows.iter().filter_map(|r| r.repo_id).collect();
    repo_ids.sort_unstable();
    repo_ids.dedup();
    let repos: Vec<(i64, String)> = sqlx::query_as(
        "SELECT r.id, o.login || '/' || r.name FROM repositories r
           JOIN users o ON o.id = r.owner_id WHERE r.id = ANY($1)",
    )
    .bind(&repo_ids)
    .fetch_all(&state.db)
    .await?;
    Ok(Names {
        users,
        repos: repos.into_iter().collect(),
    })
}

fn per_page(v: Option<u32>) -> i64 {
    i64::from(v.unwrap_or(30).clamp(1, 100))
}

/// Rebuild the request URL with `key=value` replacing any `key` and the
/// cursor parameters.
fn cursor_url(state: &AppState, uri: &Uri, key: &str, value: i64) -> String {
    let mut q: Vec<String> = uri
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let k = p.split('=').next().unwrap_or("");
            !matches!(k, "after" | "before" | "cursor")
        })
        .map(str::to_string)
        .collect();
    q.push(format!("{key}={value}"));
    format!("{}{}?{}", state.config.base_url, uri.path(), q.join("&"))
}

// ---------------------------------------------------------------------------
// Admin UI search
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct AdminParams {
    pub phrase: Option<String>,
    pub action: Option<String>,
    pub actor: Option<String>,
    pub user: Option<String>,
    pub org: Option<String>,
    pub repo: Option<String>,
    /// Inclusive lower bound (date or RFC 3339).
    pub since: Option<String>,
    /// Exclusive upper bound (date → end of that day).
    pub until: Option<String>,
    /// Id of the last entry of the previous page.
    pub cursor: Option<i64>,
    /// `desc` (default, newest first) | `asc`
    pub order: Option<String>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct EntryActor {
    pub id: Option<i64>,
    pub login: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub id: i64,
    pub action: String,
    pub actor: EntryActor,
    pub target_type: Option<String>,
    pub target_id: Option<i64>,
    pub user: Option<String>,
    pub org_id: Option<i64>,
    pub org: Option<String>,
    pub repo_id: Option<i64>,
    pub repo: Option<String>,
    pub data: Value,
    pub ip: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Serialize)]
pub struct EntriesPage {
    pub entries: Vec<Entry>,
    pub next_cursor: Option<i64>,
}

fn admin_filter(p: &AdminParams) -> ApiResult<Filter> {
    let mut f = Filter::default();
    if let Some(phrase) = &p.phrase {
        f.parse_phrase(phrase)?;
    }
    let push = |v: &Option<String>, out: &mut Vec<String>| {
        if let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            out.push(v.to_string());
        }
    };
    push(&p.action, &mut f.actions);
    push(&p.actor, &mut f.actors);
    push(&p.user, &mut f.users);
    push(&p.org, &mut f.orgs);
    push(&p.repo, &mut f.repos);
    if let Some(s) = &p.since {
        f.from =
            Some(day_start(s).ok_or_else(|| {
                ApiError::invalid_field(FieldError::invalid("AuditLog", "since"))
            })?);
    }
    if let Some(s) = &p.until {
        let t = day_start(s)
            .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("AuditLog", "until")))?;
        f.to = Some(if is_day(s) { t + Duration::days(1) } else { t });
    }
    Ok(f)
}

/// `GET /_bgh/admin/audit-log` → `{entries, next_cursor}` (+ `Link: next`).
pub async fn admin_search(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<AdminParams>,
) -> ApiResult<Response> {
    let f = admin_filter(&p)?;
    let asc = p.order.as_deref() == Some("asc");
    let limit = per_page(p.per_page);
    let mut rows = search(&state, &f, None, p.cursor, asc, limit + 1).await?;
    let has_next = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let n = names(&state, &rows).await?;
    let next_cursor = has_next.then(|| rows.last().map(|r| r.id)).flatten();
    let entries = rows
        .into_iter()
        .map(|r| Entry {
            id: r.id,
            actor: EntryActor {
                id: r.actor_id,
                login: r
                    .actor_id
                    .and_then(|id| n.users.get(&id))
                    .map(|u| u.login.clone())
                    .or(r.actor_login),
            },
            user: (r.target_type.as_deref() == Some("user"))
                .then(|| {
                    r.target_id
                        .and_then(|id| n.users.get(&id))
                        .map(|u| u.login.clone())
                })
                .flatten(),
            org: r
                .org_id
                .and_then(|id| n.users.get(&id))
                .map(|u| u.login.clone()),
            repo: r.repo_id.and_then(|id| n.repos.get(&id)).cloned(),
            action: r.action,
            target_type: r.target_type,
            target_id: r.target_id,
            org_id: r.org_id,
            repo_id: r.repo_id,
            data: r.data,
            ip: r.ip,
            created_at: r.created_at.into(),
        })
        .collect();
    let mut resp = Json(EntriesPage {
        entries,
        next_cursor,
    })
    .into_response();
    if let Some(c) = next_cursor
        && let Ok(v) = HeaderValue::from_str(&format!(
            "<{}>; rel=\"next\"",
            cursor_url(&state, &uri, "cursor", c)
        ))
    {
        resp.headers_mut().insert(header::LINK, v);
    }
    Ok(resp)
}

// ---------------------------------------------------------------------------
// GitHub shape (org / enterprise)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct GithubParams {
    pub phrase: Option<String>,
    /// `web` | `git` | `all` (only web events are recorded).
    pub include: Option<String>,
    /// Cursor: entries after this document id (in the chosen order).
    pub after: Option<String>,
    /// Cursor: entries before this document id.
    pub before: Option<String>,
    /// `desc` (default) | `asc`
    pub order: Option<String>,
    pub per_page: Option<u32>,
}

fn github_entry(r: Row, n: &Names, with_ip: bool) -> Value {
    let ms = r.created_at.timestamp_millis();
    let mut m = Map::new();
    // Extra data first so the standard fields win on collisions.
    if let Value::Object(data) = r.data {
        for (k, v) in data {
            m.insert(k, v);
        }
    }
    m.insert("@timestamp".into(), json!(ms));
    m.insert("_document_id".into(), json!(r.id.to_string()));
    m.insert("action".into(), json!(r.action));
    m.insert(
        "actor".into(),
        json!(
            r.actor_id
                .and_then(|id| n.users.get(&id))
                .map(|u| u.login.clone())
                .or(r.actor_login)
        ),
    );
    m.insert("actor_id".into(), json!(r.actor_id));
    m.insert("created_at".into(), json!(ms));
    if let Some(org) = r.org_id {
        m.insert("org".into(), json!(n.users.get(&org).map(|u| &u.login)));
        m.insert("org_id".into(), json!(org));
    }
    if let Some(repo) = r.repo_id {
        let name = n
            .repos
            .get(&repo)
            .cloned()
            .or_else(|| m.get("name").and_then(Value::as_str).map(str::to_string));
        m.insert("repo".into(), json!(name));
        m.insert("repo_id".into(), json!(repo));
    }
    if r.target_type.as_deref() == Some("user")
        && let Some(id) = r.target_id
    {
        m.insert("user".into(), json!(n.users.get(&id).map(|u| &u.login)));
        m.insert("user_id".into(), json!(id));
    }
    if with_ip && let Some(ip) = r.ip {
        m.insert("actor_ip".into(), json!(ip));
    }
    Value::Object(m)
}

async fn github_search(
    state: &AppState,
    uri: &Uri,
    p: GithubParams,
    org_id: Option<i64>,
    with_ip: bool,
) -> ApiResult<Response> {
    if p.include
        .as_deref()
        .is_some_and(|i| !matches!(i, "web" | "git" | "all"))
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "AuditLog", "include",
        )));
    }
    let mut f = Filter::default();
    if let Some(phrase) = &p.phrase {
        f.parse_phrase(phrase)?;
    }
    let mut asc = p.order.as_deref() == Some("asc");
    let parse_cursor = |s: &Option<String>| -> ApiResult<Option<i64>> {
        s.as_deref()
            .map(|c| {
                c.parse::<i64>()
                    .map_err(|_| ApiError::invalid_field(FieldError::invalid("AuditLog", "cursor")))
            })
            .transpose()
    };
    let after = parse_cursor(&p.after)?;
    let before = parse_cursor(&p.before)?;
    // `before` walks backwards: query in reverse order, then flip.
    let (cursor, reversed) = match (after, before) {
        (Some(a), _) => (Some(a), false),
        (None, Some(b)) => {
            asc = !asc;
            (Some(b), true)
        }
        _ => (None, false),
    };
    let limit = per_page(p.per_page);
    let mut rows = search(state, &f, org_id, cursor, asc, limit + 1).await?;
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    if reversed {
        rows.reverse();
    }
    let first = rows.first().map(|r| r.id);
    let last = rows.last().map(|r| r.id);
    let n = names(state, &rows).await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| github_entry(r, &n, with_ip))
        .collect();
    let mut links = Vec::new();
    let more_after = if reversed { true } else { has_more };
    let more_before = if reversed { has_more } else { cursor.is_some() };
    if more_after && let Some(l) = last {
        links.push(format!(
            "<{}>; rel=\"next\"",
            cursor_url(state, uri, "after", l)
        ));
    }
    if more_before && let Some(f) = first {
        links.push(format!(
            "<{}>; rel=\"prev\"",
            cursor_url(state, uri, "before", f)
        ));
    }
    let mut resp = Json(items).into_response();
    if !links.is_empty()
        && let Ok(v) = HeaderValue::from_str(&links.join(", "))
    {
        resp.headers_mut().insert(header::LINK, v);
    }
    Ok(resp)
}

/// `GET /orgs/{org}/audit-log`: organization owners (and site admins);
/// tokens need `read:audit_log` or `admin:org`.
pub async fn org_audit_log(
    State(state): State<AppState>,
    auth: RequireUser,
    OriginalUri(uri): OriginalUri,
    Path(org): Path<String>,
    Query(p): Query<GithubParams>,
) -> ApiResult<Response> {
    let org = common::org(&state, &org).await?;
    let role = perms::org_role(&state.db, org.id, auth.user.id).await?;
    match role {
        Some(perms::OrgRole::Admin) => {}
        _ if auth.user.site_admin => {}
        Some(_) => {
            return Err(ApiError::forbidden(
                "Must be an organization owner to view the audit log.",
            ));
        }
        None => return Err(ApiError::NotFound),
    }
    if !auth.has_scope("read:audit_log") && !auth.has_scope("admin:org") {
        auth.require_scope("read:audit_log")?;
    }
    github_search(&state, &uri, p, Some(org.id), false).await
}

/// `GET /enterprises/{enterprise}/audit-log`: the whole instance (site
/// admins). The enterprise slug is not checked: there is one per instance.
pub async fn enterprise_audit_log(
    State(state): State<AppState>,
    _auth: RequireSiteAdmin,
    OriginalUri(uri): OriginalUri,
    Path(_enterprise): Path<String>,
    Query(p): Query<GithubParams>,
) -> ApiResult<Response> {
    github_search(&state, &uri, p, None, true).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_phrase() {
        let mut f = Filter::default();
        f.parse_phrase("action:repo actor:alice repo:acme/web created:>=2024-01-02 org:acme")
            .unwrap();
        assert_eq!(f.actions, vec!["repo"]);
        assert_eq!(f.actors, vec!["alice"]);
        assert_eq!(f.repos, vec!["acme/web"]);
        assert_eq!(f.orgs, vec!["acme"]);
        assert_eq!(f.from.unwrap().to_rfc3339(), "2024-01-02T00:00:00+00:00");
        assert!(f.to.is_none());

        let mut f = Filter::default();
        f.parse_phrase("created:2024-03-05").unwrap();
        assert_eq!(f.from.unwrap().to_rfc3339(), "2024-03-05T00:00:00+00:00");
        assert_eq!(f.to.unwrap().to_rfc3339(), "2024-03-06T00:00:00+00:00");

        let mut f = Filter::default();
        f.parse_phrase("created:2024-01-01..2024-01-31").unwrap();
        assert_eq!(f.to.unwrap().to_rfc3339(), "2024-02-01T00:00:00+00:00");

        let mut f = Filter::default();
        f.parse_phrase("created:<=2024-01-01").unwrap();
        assert_eq!(f.to.unwrap().to_rfc3339(), "2024-01-02T00:00:00+00:00");

        assert!(Filter::default().parse_phrase("bogus:1").is_err());
        assert!(Filter::default().parse_phrase("created:yesterday").is_err());
    }
}
