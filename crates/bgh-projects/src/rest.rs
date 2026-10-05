//! GitHub's REST "Projects" API (`projectsV2`), per docs.github.com:
//! projects, fields and items of an organization (`/orgs/{org}/projectsV2`)
//! or a user (`/users/{username}/projectsV2`).
//!
//! Lists use GitHub's cursor pagination (`before` / `after` / `per_page`,
//! `Link` header). Writes go through the same service functions as the
//! private web API, so permissions, sync records and workflows are shared.
//! The item filter ([`filter_items`]) is also used by the GraphQL
//! `ProjectV2.items(query:)` argument.

use std::collections::{HashMap, HashSet};

use axum::extract::{FromRequestParts, OriginalUri, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bgh_core::mail::escape_html;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::time::ts;
use bgh_core::urls::Urls;
use bgh_core::views;
use bgh_issues::json::{self as issue_json, BodyFormat, IssueOpts};
use chrono::{Duration, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::access::{ProjectAccess, owner_role};
use crate::compact::readable_repos;
use crate::filter::tokens;
use crate::items::{self, CreateBody, DraftInput, UpdateBody};
use crate::model::*;

// ---------------------------------------------------------------------------
// Owners and URLs
// ---------------------------------------------------------------------------

/// Resolve the owner of a `/orgs/{org}` (`org = true`) or
/// `/users/{username}` route; the wrong kind of account is a 404.
async fn load_owner(state: &AppState, org: bool, login: &str) -> ApiResult<db::User> {
    let user = db::User::find_by_login(&state.db, login)
        .await?
        .ok_or(ApiError::NotFound)?;
    let ok = if org {
        user.is_org()
    } else {
        user.kind == "User"
    };
    if ok {
        Ok(user)
    } else {
        Err(ApiError::NotFound)
    }
}

async fn load_project(
    state: &AppState,
    auth: Option<&AuthContext>,
    org: bool,
    login: &str,
    number: i64,
) -> ApiResult<ProjectAccess> {
    let owner = load_owner(state, org, login).await?;
    ProjectAccess::load_by_number(state, auth, &owner.login, number).await
}

/// `{api}/orgs/{org}/projectsV2/{number}` or `{api}/users/{login}/projectsV2/{number}`.
pub fn project_api_url(urls: &Urls, owner: &db::User, number: i64) -> String {
    let kind = if owner.is_org() { "orgs" } else { "users" };
    urls.api(&format!("/{kind}/{}/projectsV2/{number}", owner.login))
}

/// The project's web page (`/orgs/{org}/projects/{n}` or `/users/{login}/projects/{n}`).
pub fn project_html_path(owner: &db::User, number: i64) -> String {
    let kind = if owner.is_org() { "orgs" } else { "users" };
    format!("/{kind}/{}/projects/{number}", owner.login)
}

// ---------------------------------------------------------------------------
// Cursor pagination
// ---------------------------------------------------------------------------

/// `before` / `after` / `per_page` (default 30, max 100) with a `Link`
/// header carrying opaque cursors, like GitHub's projectsV2 endpoints.
#[derive(Debug, Clone)]
pub struct CursorPagination {
    per_page: i64,
    after: Option<i64>,
    before: Option<i64>,
    base_url: String,
    other_params: Vec<(String, String)>,
}

fn encode_cursor(pos: i64) -> String {
    URL_SAFE_NO_PAD.encode(format!("pos:{pos}"))
}

fn decode_cursor(c: &str) -> ApiResult<i64> {
    URL_SAFE_NO_PAD
        .decode(c.trim_end_matches('='))
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|s| s.strip_prefix("pos:").and_then(|n| n.parse().ok()))
        .filter(|n: &i64| *n >= 0)
        .ok_or_else(|| ApiError::unprocessable(format!("`{c}` is not a valid cursor")))
}

impl CursorPagination {
    pub fn from_parts(external_base: &str, path: &str, query: Option<&str>) -> ApiResult<Self> {
        let mut out = Self {
            per_page: 30,
            after: None,
            before: None,
            base_url: format!("{external_base}{path}"),
            other_params: vec![],
        };
        for pair in query.unwrap_or("").split('&').filter(|s| !s.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            match k {
                "per_page" => {
                    if let Ok(n) = v.parse::<i64>() {
                        out.per_page = n.clamp(1, 100);
                    }
                }
                "after" if !v.is_empty() => out.after = Some(decode_cursor(v)?),
                "before" if !v.is_empty() => out.before = Some(decode_cursor(v)?),
                "after" | "before" | "page" => {}
                _ => out.other_params.push((k.to_string(), v.to_string())),
            }
        }
        Ok(out)
    }

    /// Slice one page out of the full (filtered, ordered) list.
    pub fn page<T>(&self, all: Vec<T>) -> (Vec<T>, Option<String>) {
        let total = all.len() as i64;
        let (offset, limit) = match self.before {
            Some(b) => {
                let end = (b - 1).clamp(0, total);
                let start = (end - self.per_page).max(0);
                (start, end - start)
            }
            None => {
                let start = self.after.unwrap_or(0).min(total);
                (start, self.per_page)
            }
        };
        let items: Vec<T> = all
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        let end = offset + items.len() as i64;
        let mut links = Vec::new();
        if offset > 0 {
            links.push(format!(
                "<{}>; rel=\"prev\"",
                self.url("before", &encode_cursor(offset + 1))
            ));
        }
        if end < total {
            links.push(format!(
                "<{}>; rel=\"next\"",
                self.url("after", &encode_cursor(end))
            ));
        }
        (items, (!links.is_empty()).then(|| links.join(", ")))
    }

    fn url(&self, key: &str, cursor: &str) -> String {
        let mut q: Vec<String> = self
            .other_params
            .iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    k.clone()
                } else {
                    format!("{k}={v}")
                }
            })
            .collect();
        q.push(format!("per_page={}", self.per_page));
        q.push(format!("{key}={cursor}"));
        format!("{}?{}", self.base_url, q.join("&"))
    }
}

impl FromRequestParts<AppState> for CursorPagination {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let uri = parts
            .extensions
            .get::<OriginalUri>()
            .map(|u| u.0.clone())
            .unwrap_or_else(|| parts.uri.clone());
        Self::from_parts(&state.config.base_url, uri.path(), uri.query())
    }
}

/// A JSON array with an optional `Link` header.
fn paged(items: Vec<Value>, link: Option<String>) -> Response {
    let mut resp = Json(items).into_response();
    if let Some(link) = link
        && let Ok(v) = HeaderValue::from_str(&link)
    {
        resp.headers_mut().insert(header::LINK, v);
    }
    resp
}

/// Field ids from `fields=1,2` or `fields[]=1&fields[]=2`.
fn selected_field_ids(query: Option<&str>) -> Option<Vec<i64>> {
    let mut ids = Vec::new();
    let mut any = false;
    for pair in query.unwrap_or("").split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if matches!(k, "fields" | "fields[]" | "fields%5B%5D" | "fields%5b%5d") {
            any = true;
            ids.extend(
                v.split([',', '%'])
                    .map(|s| s.trim_start_matches("2C").trim_start_matches("2c"))
                    .filter_map(|s| s.parse::<i64>().ok()),
            );
        }
    }
    any.then_some(ids)
}

// ---------------------------------------------------------------------------
// JSON shapes
// ---------------------------------------------------------------------------

fn rich(s: &str) -> Value {
    json!({"raw": s, "html": escape_html(s)})
}

/// `projects-v2`.
pub fn project_json(
    urls: &Urls,
    p: &ProjectRow,
    owner: &db::User,
    creator: Option<&db::User>,
) -> Value {
    json!({
        "id": p.id,
        "node_id": node_id::encode(NodeType::ProjectV2, p.id),
        "owner": api::SimpleUser::new(urls, owner),
        "creator": api::SimpleUser::or_ghost(urls, creator),
        "title": p.title,
        "description": p.readme,
        "public": p.public,
        "closed_at": ts(p.closed_at),
        "created_at": Timestamp(p.created_at),
        "updated_at": Timestamp(p.updated_at),
        "number": p.number,
        "short_description": p.short_description,
        "deleted_at": null,
        "deleted_by": null,
        "state": if p.closed { "closed" } else { "open" },
        "latest_status_update": null,
        "is_template": false,
    })
}

/// REST `data_type` (the Status field is a single select).
pub fn rest_data_type(f: &FieldRow) -> &str {
    match f.data_type.as_str() {
        "status" => "single_select",
        t => t,
    }
}

/// Whether an iteration has ended (`start + duration <= today`).
pub fn iteration_completed(start: &str, duration: i64) -> bool {
    NaiveDate::parse_from_str(start, "%Y-%m-%d")
        .map(|s| s + Duration::days(duration) <= Utc::now().date_naive())
        .unwrap_or(false)
}

fn option_json(o: &Value) -> Value {
    let s = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    json!({
        "id": s("id"),
        "name": rich(&s("name")),
        "color": s("color"),
        "description": rich(&s("description")),
    })
}

fn iteration_json(i: &Value, with_completed: bool) -> Value {
    let start = i.get("startDate").and_then(Value::as_str).unwrap_or("");
    let duration = i.get("duration").and_then(Value::as_i64).unwrap_or(0);
    let mut v = json!({
        "id": i.get("id").and_then(Value::as_str).unwrap_or(""),
        "title": rich(i.get("title").and_then(Value::as_str).unwrap_or("")),
        "start_date": start,
        "duration": duration,
    });
    if with_completed {
        v["completed"] = json!(iteration_completed(start, duration));
    }
    v
}

/// `projects-v2-field`.
pub fn field_json(project_url: &str, f: &FieldRow) -> Value {
    let mut v = json!({
        "id": f.id,
        "node_id": node_id::encode(NodeType::ProjectV2Field, f.id),
        "project_url": project_url,
        "name": f.name,
        "data_type": rest_data_type(f),
        "created_at": Timestamp(f.created_at),
        "updated_at": Timestamp(f.updated_at),
    });
    if f.is_select() {
        let opts: Vec<Value> = f
            .options
            .as_ref()
            .and_then(Value::as_array)
            .map(|a| a.iter().map(option_json).collect())
            .unwrap_or_default();
        v["options"] = json!(opts);
    }
    if f.data_type == "iteration" {
        let cfg = f.iterations.clone().unwrap_or(Value::Null);
        let start = cfg.get("startDate").and_then(Value::as_str).unwrap_or("");
        let start_day = NaiveDate::parse_from_str(start, "%Y-%m-%d")
            .map(|d| chrono::Datelike::weekday(&d).number_from_monday())
            .unwrap_or(1);
        let its: Vec<Value> = cfg
            .get("iterations")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|i| iteration_json(i, true)).collect())
            .unwrap_or_default();
        v["configuration"] = json!({
            "start_day": start_day,
            "duration": cfg.get("duration").and_then(Value::as_i64).unwrap_or(14),
            "iterations": its,
        });
    }
    v
}

/// What rendering a page of items needs: issue contents (readable
/// repositories only) and users (creators, draft assignees).
pub struct ItemContext {
    /// Rendered REST `issue` JSON (with `repository`) per issue id.
    pub issues: HashMap<i64, Value>,
    pub users: HashMap<i64, db::User>,
}

impl ItemContext {
    pub async fn load(
        state: &AppState,
        auth: Option<&AuthContext>,
        items: &[ItemRow],
    ) -> ApiResult<Self> {
        let issue_ids: Vec<i64> = items.iter().filter_map(|i| i.issue_id).collect();
        let mut issues = HashMap::new();
        if !issue_ids.is_empty() {
            let rows: Vec<db::Issue> = sqlx::query_as(&format!(
                "SELECT {} FROM issues WHERE id = ANY($1) ORDER BY id",
                db::Issue::COLUMNS
            ))
            .bind(&issue_ids)
            .fetch_all(&state.db)
            .await?;
            let mut repo_ids: Vec<i64> = rows.iter().map(|r| r.repo_id).collect();
            repo_ids.sort_unstable();
            repo_ids.dedup();
            let readable: HashSet<i64> = readable_repos(state, auth, &repo_ids)
                .await?
                .iter()
                .map(|r| r.id)
                .collect();
            let rows: Vec<db::Issue> = rows
                .into_iter()
                .filter(|r| readable.contains(&r.repo_id))
                .collect();
            let repos = issue_json::load_repos(state, readable.iter().copied()).await?;
            let mut rendered =
                issue_json::issues(state, BodyFormat::Raw, &rows, &repos, IssueOpts::default())
                    .await?;
            issue_json::attach_repositories(state, auth, &repos, &mut rendered, &rows).await?;
            for issue in rendered {
                let id = issue.id;
                issues.insert(id, serde_json::to_value(issue).unwrap_or(Value::Null));
            }
        }
        let mut user_ids: Vec<Option<i64>> = items.iter().map(|i| i.creator_id).collect();
        for i in items {
            user_ids.extend(i.assignee_ids.iter().copied().map(Some));
        }
        let users = views::users_by_id(state, user_ids).await?;
        Ok(Self { issues, users })
    }

    fn user(&self, id: Option<i64>) -> Option<&db::User> {
        id.and_then(|id| self.users.get(&id))
    }
}

fn draft_json(urls: &Urls, item: &ItemRow, cx: &ItemContext) -> Value {
    json!({
        "id": item.id,
        "node_id": node_id::encode(NodeType::DraftIssue, item.id),
        "title": item.title,
        "body": item.body,
        "user": cx.user(item.creator_id).map(|u| api::SimpleUser::new(urls, u)),
        "created_at": Timestamp(item.created_at),
        "updated_at": Timestamp(item.updated_at),
    })
}

/// The value of `field` on `item` (REST shape; `null` when unset).
fn value_json(urls: &Urls, f: &FieldRow, item: &ItemRow, cx: &ItemContext) -> Value {
    let issue = item.issue_id.and_then(|id| cx.issues.get(&id));
    let get = |k: &str| issue.and_then(|i| i.get(k)).cloned();
    let stored = item.field_values.get(f.id.to_string());
    match f.data_type.as_str() {
        "title" => match (item.is_draft(), issue) {
            (true, _) => rich(item.title.as_deref().unwrap_or("")),
            (false, Some(i)) => {
                let title = i["title"].as_str().unwrap_or("");
                let mut v = rich(title);
                v["number"] = i["number"].clone();
                v["url"] = i["html_url"].clone();
                v["issue_id"] = i["id"].clone();
                v["state"] = i["state"].clone();
                v["state_reason"] = i["state_reason"].clone();
                v["is_draft"] = json!(i["draft"].as_bool().unwrap_or(false));
                v
            }
            (false, None) => Value::Null,
        },
        "assignees" if item.is_draft() => json!(
            item.assignee_ids
                .iter()
                .filter_map(|id| cx.users.get(id))
                .map(|u| api::SimpleUser::new(urls, u))
                .collect::<Vec<_>>()
        ),
        "assignees" => get("assignees").unwrap_or_else(|| json!([])),
        "labels" => get("labels").unwrap_or_else(|| json!([])),
        "milestone" => get("milestone").unwrap_or(Value::Null),
        "repository" => get("repository").unwrap_or(Value::Null),
        "single_select" | "status" => {
            let id = stored.and_then(Value::as_str);
            f.options
                .as_ref()
                .and_then(Value::as_array)
                .and_then(|a| a.iter().find(|o| o["id"].as_str() == id && id.is_some()))
                .map(option_json)
                .unwrap_or(Value::Null)
        }
        "iteration" => {
            let id = stored.and_then(Value::as_str);
            f.iterations
                .as_ref()
                .and_then(|c| c.get("iterations"))
                .and_then(Value::as_array)
                .and_then(|a| a.iter().find(|i| i["id"].as_str() == id && id.is_some()))
                .map(|i| iteration_json(i, false))
                .unwrap_or(Value::Null)
        }
        _ => stored.cloned().unwrap_or(Value::Null),
    }
}

/// `projects-v2-item-with-content` (`fields` = the selected fields) or,
/// with `fields: None`, `projects-v2-item-simple`.
pub fn item_json(
    urls: &Urls,
    project_url: &str,
    item: &ItemRow,
    cx: &ItemContext,
    fields: Option<&[&FieldRow]>,
) -> Value {
    let content = if item.is_draft() {
        draft_json(urls, item, cx)
    } else {
        item.issue_id
            .and_then(|id| cx.issues.get(&id))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mut v = json!({
        "id": item.id,
        "node_id": node_id::encode(NodeType::ProjectV2Item, item.id),
        "project_url": project_url,
        "content_type": item.content_type,
        "content": content,
        "creator": api::SimpleUser::or_ghost(urls, cx.user(item.creator_id)),
        "created_at": Timestamp(item.created_at),
        "updated_at": Timestamp(item.updated_at),
        "archived_at": ts(item.archived_at),
        "item_url": format!("{project_url}/items/{}", item.id),
    });
    if let Some(fields) = fields {
        v["fields"] = json!(
            fields
                .iter()
                .map(|f| json!({
                    "id": f.id,
                    "name": f.name,
                    "data_type": rest_data_type(f),
                    "value": value_json(urls, f, item, cx),
                }))
                .collect::<Vec<_>>()
        );
    }
    v
}

// ---------------------------------------------------------------------------
// Loading and filtering
// ---------------------------------------------------------------------------

pub async fn project_fields(db: &sqlx::PgPool, project_id: i64) -> ApiResult<Vec<FieldRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {} FROM project_fields WHERE project_id = $1 ORDER BY position, id",
        FieldRow::COLUMNS
    ))
    .bind(project_id)
    .fetch_all(db)
    .await?)
}

/// The project's items in display order (`position`), filtered by `query`
/// (the project filter syntax: `is:issue|pr|draft|open|closed|archived`,
/// `label:`, `assignee:`, `repo:`, `milestone:`, `no:<field>`, `<field
/// name>:<value>` for custom fields, plain words in the title; `-`
/// negates, commas OR values). Archived items are only included with
/// `is:archived` (or `include_archived`).
pub async fn filter_items(
    state: &AppState,
    auth: Option<&AuthContext>,
    project_id: i64,
    query: Option<&str>,
    include_archived: bool,
) -> ApiResult<Vec<ItemRow>> {
    let query = query.map(str::trim).filter(|q| !q.is_empty());
    let toks = query.map(tokens).unwrap_or_default();
    let wants_archived = include_archived
        || toks
            .iter()
            .any(|t| t.to_lowercase().starts_with("is:") && t.to_lowercase().contains("archived"));
    let items: Vec<ItemRow> = sqlx::query_as(&format!(
        "{} WHERE i.project_id = $1 AND ($2 OR NOT i.archived) ORDER BY i.position, i.id",
        ItemRow::SELECT
    ))
    .bind(project_id)
    .bind(wants_archived)
    .fetch_all(&state.db)
    .await?;
    if toks.is_empty() {
        return Ok(items);
    }
    let fields = project_fields(&state.db, project_id).await?;
    let cx = ItemContext::load(state, auth, &items).await?;
    Ok(items
        .into_iter()
        .filter(|i| item_matches(&toks, i, &fields, &cx))
        .collect())
}

fn item_matches(toks: &[String], item: &ItemRow, fields: &[FieldRow], cx: &ItemContext) -> bool {
    let issue = item.issue_id.and_then(|id| cx.issues.get(&id));
    let title = if item.is_draft() {
        item.title.clone().unwrap_or_default()
    } else {
        issue
            .and_then(|i| i["title"].as_str())
            .unwrap_or("")
            .to_string()
    }
    .to_lowercase();
    let names = |k: &str, sub: &str| -> Vec<String> {
        issue
            .and_then(|i| i[k].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x[sub].as_str().map(str::to_lowercase))
                    .collect()
            })
            .unwrap_or_default()
    };
    let assignees: Vec<String> = if item.is_draft() {
        item.assignee_ids
            .iter()
            .filter_map(|id| cx.users.get(id))
            .map(|u| u.login.to_lowercase())
            .collect()
    } else {
        names("assignees", "login")
    };
    let labels = names("labels", "name");
    let open = issue.is_none_or(|i| i["state"] == "open");
    let repo = issue
        .and_then(|i| i["repository"]["full_name"].as_str())
        .map(str::to_lowercase);
    let milestone = issue
        .and_then(|i| i["milestone"]["title"].as_str())
        .map(str::to_lowercase);
    // Display value of a custom field (option name, iteration title, raw value).
    let custom = |f: &FieldRow| -> Option<String> {
        let v = item.field_values.get(f.id.to_string())?;
        let id = v.as_str();
        let found = |list: Option<&Value>, key: &str| {
            list.and_then(Value::as_array)
                .and_then(|a| a.iter().find(|o| o["id"].as_str() == id))
                .and_then(|o| o[key].as_str().map(str::to_lowercase))
        };
        match f.data_type.as_str() {
            "single_select" | "status" => found(f.options.as_ref(), "name"),
            "iteration" => found(
                f.iterations.as_ref().and_then(|c| c.get("iterations")),
                "title",
            ),
            _ => Some(match v {
                Value::String(s) => s.to_lowercase(),
                Value::Number(n) => match n.as_f64() {
                    Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
                    _ => n.to_string(),
                },
                other => other.to_string(),
            }),
        }
    };
    toks.iter().all(|tok| {
        let (neg, tok) = match tok.strip_prefix('-') {
            Some(t) => (true, t),
            None => (false, tok.as_str()),
        };
        let hit = match tok.split_once(':') {
            Some((key, vals)) => {
                let vals: Vec<String> = vals
                    .split(',')
                    .map(|v| v.trim().to_lowercase())
                    .filter(|v| !v.is_empty())
                    .collect();
                let key = key.to_lowercase();
                let any = |have: &[String]| vals.iter().any(|v| have.contains(v));
                match key.as_str() {
                    "is" => vals.iter().any(|v| match v.as_str() {
                        "issue" => item.content_type == "Issue",
                        "pr" => item.content_type == "PullRequest",
                        "draft" => item.is_draft(),
                        "open" => open,
                        "closed" => !open,
                        "archived" => item.archived,
                        _ => true,
                    }),
                    "label" | "labels" => any(&labels),
                    "assignee" | "assignees" => any(&assignees),
                    "repo" | "repository" => repo.as_ref().is_some_and(|r| {
                        vals.iter()
                            .any(|v| r == v || r.rsplit('/').next() == Some(v.as_str()))
                    }),
                    "milestone" => milestone.as_ref().is_some_and(|m| vals.contains(m)),
                    "no" => vals.iter().all(|v| match v.as_str() {
                        "label" | "labels" => labels.is_empty(),
                        "assignee" | "assignees" => assignees.is_empty(),
                        "milestone" => milestone.is_none(),
                        name => fields
                            .iter()
                            .find(|f| f.name.to_lowercase() == name)
                            .is_some_and(|f| custom(f).is_none()),
                    }),
                    name => match fields.iter().find(|f| f.name.to_lowercase() == name) {
                        Some(f) => custom(f).is_some_and(|v| vals.contains(&v)),
                        None => true,
                    },
                }
            }
            None => title.contains(&tok.to_lowercase()),
        };
        hit != neg
    })
}

// ---------------------------------------------------------------------------
// Handlers (`ORG` selects /orgs/{org} vs /users/{username})
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub q: Option<String>,
}

/// `GET /orgs/{org}/projectsV2`, `GET /users/{username}/projectsV2`
pub async fn list_projects<const ORG: bool>(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: CursorPagination,
    Path(login): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    let owner = load_owner(&state, ORG, &login).await?;
    let role = owner_role(&state, auth.as_ref(), &owner).await?;
    let mut closed: Option<bool> = None;
    let mut words = Vec::new();
    for tok in q.q.as_deref().map(tokens).unwrap_or_default() {
        match tok.to_lowercase().as_str() {
            "is:open" => closed = Some(false),
            "is:closed" => closed = Some(true),
            t if t.contains(':') => {}
            _ => words.push(tok),
        }
    }
    let title = (!words.is_empty()).then(|| words.join(" "));
    let rows: Vec<ProjectRow> = sqlx::query_as(&format!(
        "{} WHERE p.owner_id = $1 AND ($2::bool OR p.public)
           AND ($3::bool IS NULL OR p.closed = $3)
           AND ($4::text IS NULL OR p.title ILIKE '%' || $4 || '%')
         ORDER BY p.number DESC",
        ProjectRow::SELECT
    ))
    .bind(owner.id)
    .bind(role.is_some())
    .bind(closed)
    .bind(title)
    .fetch_all(&state.db)
    .await?;
    let (rows, link) = p.page(rows);
    let creators = views::users_by_id(&state, rows.iter().map(|r| r.creator_id)).await?;
    let items = rows
        .iter()
        .map(|r| {
            project_json(
                &state.urls,
                r,
                &owner,
                r.creator_id.and_then(|c| creators.get(&c)),
            )
        })
        .collect();
    Ok(paged(items, link))
}

/// `GET /orgs/{org}/projectsV2/{project_number}` (and `/users/...`)
pub async fn get_project<const ORG: bool>(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((login, number)): Path<(String, i64)>,
) -> ApiResult<Json<Value>> {
    let access = load_project(&state, auth.as_ref(), ORG, &login, number).await?;
    let creator = match access.project.creator_id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    Ok(Json(project_json(
        &state.urls,
        &access.project,
        &access.owner,
        creator.as_ref(),
    )))
}

/// `GET .../projectsV2/{project_number}/fields`
pub async fn list_fields<const ORG: bool>(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: CursorPagination,
    Path((login, number)): Path<(String, i64)>,
) -> ApiResult<Response> {
    let access = load_project(&state, auth.as_ref(), ORG, &login, number).await?;
    let url = project_api_url(&state.urls, &access.owner, number);
    let fields = project_fields(&state.db, access.id()).await?;
    let (fields, link) = p.page(fields);
    Ok(paged(
        fields.iter().map(|f| field_json(&url, f)).collect(),
        link,
    ))
}

/// `GET .../projectsV2/{project_number}/fields/{field_id}`
pub async fn get_field<const ORG: bool>(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((login, number, field_id)): Path<(String, i64, i64)>,
) -> ApiResult<Json<Value>> {
    let access = load_project(&state, auth.as_ref(), ORG, &login, number).await?;
    let field: FieldRow = sqlx::query_as(&format!(
        "SELECT {} FROM project_fields WHERE id = $1 AND project_id = $2",
        FieldRow::COLUMNS
    ))
    .bind(field_id)
    .bind(access.id())
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    let url = project_api_url(&state.urls, &access.owner, number);
    Ok(Json(field_json(&url, &field)))
}

/// Fields to render: the requested ids (in request order, unknown ids
/// ignored) or just the Title field.
fn pick_fields<'a>(all: &'a [FieldRow], ids: Option<&[i64]>) -> Vec<&'a FieldRow> {
    match ids {
        Some(ids) => ids
            .iter()
            .filter_map(|id| all.iter().find(|f| f.id == *id))
            .collect(),
        None => all.iter().filter(|f| f.data_type == "title").collect(),
    }
}

async fn render_items(
    state: &AppState,
    auth: Option<&AuthContext>,
    access: &ProjectAccess,
    items: &[ItemRow],
    field_ids: Option<&[i64]>,
) -> ApiResult<Vec<Value>> {
    let all = project_fields(&state.db, access.id()).await?;
    let fields = pick_fields(&all, field_ids);
    let cx = ItemContext::load(state, auth, items).await?;
    let url = project_api_url(&state.urls, &access.owner, access.project.number);
    Ok(items
        .iter()
        .map(|i| item_json(&state.urls, &url, i, &cx, Some(&fields)))
        .collect())
}

async fn load_item(state: &AppState, project_id: i64, item_id: i64) -> ApiResult<ItemRow> {
    sqlx::query_as(&format!(
        "{} WHERE i.id = $1 AND i.project_id = $2",
        ItemRow::SELECT
    ))
    .bind(item_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `GET .../projectsV2/{project_number}/items`
pub async fn list_items<const ORG: bool>(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: CursorPagination,
    Path((login, number)): Path<(String, i64)>,
    Query(q): Query<ListQuery>,
    uri: OriginalUri,
) -> ApiResult<Response> {
    let access = load_project(&state, auth.as_ref(), ORG, &login, number).await?;
    let items = filter_items(&state, auth.as_ref(), access.id(), q.q.as_deref(), false).await?;
    let (items, link) = p.page(items);
    let ids = selected_field_ids(uri.0.query());
    let out = render_items(&state, auth.as_ref(), &access, &items, ids.as_deref()).await?;
    Ok(paged(out, link))
}

/// `GET .../projectsV2/{project_number}/items/{item_id}`
pub async fn get_item<const ORG: bool>(
    State(state): State<AppState>,
    auth: MaybeUser,
    Path((login, number, item_id)): Path<(String, i64, i64)>,
    uri: OriginalUri,
) -> ApiResult<Json<Value>> {
    let access = load_project(&state, auth.as_ref(), ORG, &login, number).await?;
    let item = load_item(&state, access.id(), item_id).await?;
    let ids = selected_field_ids(uri.0.query());
    let mut out = render_items(&state, auth.as_ref(), &access, &[item], ids.as_deref()).await?;
    Ok(Json(out.pop().unwrap_or(Value::Null)))
}

#[derive(Debug, Deserialize)]
pub struct AddItemBody {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub id: Option<i64>,
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub number: Option<i64>,
}

async fn simple_item(
    state: &AppState,
    auth: &AuthContext,
    access: &ProjectAccess,
    item: &ItemRow,
) -> ApiResult<Value> {
    let cx = ItemContext::load(state, Some(auth), std::slice::from_ref(item)).await?;
    let url = project_api_url(&state.urls, &access.owner, access.project.number);
    Ok(item_json(&state.urls, &url, item, &cx, None))
}

/// `POST .../projectsV2/{project_number}/items` (`{type, id}` or
/// `{type, owner, repo, number}`) → `201` `projects-v2-item-simple`.
pub async fn add_item<const ORG: bool>(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((login, number)): Path<(String, i64)>,
    Json(body): Json<AddItemBody>,
) -> ApiResult<Response> {
    let access = load_project(&state, Some(&auth), ORG, &login, number).await?;
    let kind = body.kind.as_deref().ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("ProjectV2Item", "type"))
    })?;
    if !matches!(kind, "Issue" | "PullRequest") {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "ProjectV2Item",
            "type",
        )));
    }
    if body.id.is_none() && (body.owner.is_none() || body.repo.is_none() || body.number.is_none()) {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "ProjectV2Item",
            "id",
        )));
    }
    if let Some(id) = body.id {
        let is_pr: Option<bool> =
            sqlx::query_scalar("SELECT is_pull_request FROM issues WHERE id = $1")
                .bind(id)
                .fetch_optional(&state.db)
                .await?;
        if is_pr.is_some_and(|pr| pr != (kind == "PullRequest")) {
            return Err(ApiError::invalid_field(FieldError::custom(
                "ProjectV2Item",
                "type",
                format!("{id} is not a {kind}"),
            )));
        }
    }
    let (item, created) = items::add_item(
        &state,
        &auth,
        access.id(),
        CreateBody {
            issue_id: body.id,
            owner: body.owner,
            repo: body.repo,
            number: body.number,
            ..Default::default()
        },
    )
    .await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(simple_item(&state, &auth, &access, &item).await?),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub struct DraftBody {
    pub title: Option<String>,
    pub body: Option<String>,
}

async fn create_draft(
    state: &AppState,
    auth: &AuthContext,
    access: ProjectAccess,
    body: DraftBody,
) -> ApiResult<Response> {
    let (item, _) = items::add_item(
        state,
        auth,
        access.id(),
        CreateBody {
            draft: Some(DraftInput {
                title: body.title,
                body: body.body,
            }),
            ..Default::default()
        },
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(simple_item(state, auth, &access, &item).await?),
    )
        .into_response())
}

/// `POST /orgs/{org}/projectsV2/{project_number}/drafts`
pub async fn create_org_draft(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((login, number)): Path<(String, i64)>,
    Json(body): Json<DraftBody>,
) -> ApiResult<Response> {
    let access = load_project(&state, Some(&auth), true, &login, number).await?;
    create_draft(&state, &auth, access, body).await
}

/// `POST /user/{user_id}/projectsV2/{project_number}/drafts`
pub async fn create_user_draft(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((user_id, number)): Path<(i64, i64)>,
    Json(body): Json<DraftBody>,
) -> ApiResult<Response> {
    let user = db::User::find(&state.db, user_id)
        .await?
        .filter(|u| u.kind == "User")
        .ok_or(ApiError::NotFound)?;
    let access = load_project(&state, Some(&auth), false, &user.login, number).await?;
    create_draft(&state, &auth, access, body).await
}

#[derive(Debug, Deserialize)]
pub struct FieldUpdate {
    pub id: Option<i64>,
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Deserialize)]
pub struct UpdateItemBody {
    pub fields: Option<Vec<FieldUpdate>>,
}

/// `PATCH .../projectsV2/{project_number}/items/{item_id}` (`{fields: [{id, value}]}`)
pub async fn update_item<const ORG: bool>(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((login, number, item_id)): Path<(String, i64, i64)>,
    Json(body): Json<UpdateItemBody>,
) -> ApiResult<Json<Value>> {
    let access = load_project(&state, Some(&auth), ORG, &login, number).await?;
    let updates = body.fields.ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("ProjectV2Item", "fields"))
    })?;
    let all = project_fields(&state.db, access.id()).await?;
    let mut values = HashMap::new();
    let mut ids = Vec::new();
    for u in updates {
        let id = u.id.ok_or_else(|| {
            ApiError::invalid_field(FieldError::missing_field("ProjectV2ItemFieldValue", "id"))
        })?;
        // GitHub accepts numbers as strings and vice versa.
        let value = match (all.iter().find(|f| f.id == id), u.value) {
            (Some(f), Value::String(s)) if f.data_type == "number" => s
                .trim()
                .parse::<f64>()
                .map(|n| json!(n))
                .unwrap_or(Value::String(s)),
            (_, v) => v,
        };
        values.insert(id.to_string(), value);
        ids.push(id);
    }
    let item = items::update_item(
        &state,
        &auth,
        access.id(),
        item_id,
        UpdateBody {
            values: Some(values),
            ..Default::default()
        },
    )
    .await?;
    let out = render_items(&state, Some(&auth), &access, &[item], Some(&ids)).await?;
    Ok(Json(out.into_iter().next().unwrap_or(Value::Null)))
}

/// `DELETE .../projectsV2/{project_number}/items/{item_id}`
pub async fn delete_item<const ORG: bool>(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((login, number, item_id)): Path<(String, i64, i64)>,
) -> ApiResult<StatusCode> {
    let access = load_project(&state, Some(&auth), ORG, &login, number).await?;
    items::delete_item(&state, &auth, access.id(), item_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
