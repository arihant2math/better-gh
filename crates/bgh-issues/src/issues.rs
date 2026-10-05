//! Issues: create, get, update, list (repository and cross-repository).

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bgh_core::perms::RepoAccess;
use bgh_core::prelude::*;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};

use crate::json::{self, BodyFormat, IssueOpts, RepoInfo};
use crate::{refs, service};

/// Deserialize a field that distinguishes "absent" (`None`) from `null`
/// (`Some(None)`); use with `#[serde(default, deserialize_with = "double")]`.
pub fn double<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(d)?))
}

/// 410 for repositories with issues disabled.
pub fn require_issues_enabled(access: &RepoAccess) -> ApiResult<()> {
    if access.repo.has_issues {
        Ok(())
    } else {
        Err(ApiError::Gone("Issues are disabled for this repo".into()))
    }
}

/// Load `{owner}/{repo}` issue `number` for the caller (404 without read).
pub async fn load(
    state: &AppState,
    auth: Option<&AuthContext>,
    owner: &str,
    repo: &str,
    number: i64,
) -> ApiResult<(RepoAccess, db::Issue)> {
    let access = RepoAccess::load(state, auth, owner, repo).await?;
    let issue = service::find_issue(&state.db, access.repo.id, number).await?;
    if !issue.is_pull_request {
        require_issues_enabled(&access)?;
    }
    Ok((access, issue))
}

/// Title from a JSON string or number (GitHub accepts both).
fn title_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn validate_title(v: Option<&Value>) -> ApiResult<String> {
    let title = v.and_then(title_of).unwrap_or_default();
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Issue", "title",
        )));
    }
    if title.chars().count() > 256 {
        return Err(ApiError::invalid_field(FieldError::custom(
            "Issue",
            "title",
            "title is too long (maximum is 256 characters)",
        )));
    }
    Ok(title)
}

/// Label names from `["bug", {"name": "x"}]`.
pub fn label_names(values: &[Value]) -> ApiResult<Vec<String>> {
    values
        .iter()
        .map(|v| match v {
            Value::String(s) => Ok(s.clone()),
            Value::Object(o) => o
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Label", "name"))),
            _ => Err(ApiError::invalid_field(FieldError::invalid(
                "Label", "name",
            ))),
        })
        .collect()
}

/// Resolve a milestone given by number (JSON number or numeric string).
pub async fn resolve_milestone(
    db: impl sqlx::PgExecutor<'_>,
    repo_id: i64,
    v: &Value,
) -> ApiResult<Option<db::Milestone>> {
    let number = match v {
        Value::Null => return Ok(None),
        Value::Number(n) => n.as_i64(),
        Value::String(s) if s.is_empty() || s == "none" => return Ok(None),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Issue", "milestone")))?;
    let m: Option<db::Milestone> = sqlx::query_as(&format!(
        "SELECT {} FROM milestones WHERE repo_id = $1 AND number = $2",
        db::Milestone::COLUMNS
    ))
    .bind(repo_id)
    .bind(number)
    .fetch_optional(db)
    .await?;
    m.map(Some)
        .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Issue", "milestone")))
}

/// Resolve assignee logins to assignable users; 422 for unknown or
/// non-assignable logins.
pub async fn resolve_assignees(
    state: &AppState,
    repo: &db::Repository,
    logins: &[String],
    resource: &str,
) -> ApiResult<Vec<db::User>> {
    let mut out: Vec<db::User> = Vec::new();
    for login in logins.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        if out.iter().any(|u| u.login.eq_ignore_ascii_case(login)) {
            continue;
        }
        let user = db::User::find_by_login(&state.db, login).await?;
        match user {
            Some(u) if service::is_assignable(state, repo, &u).await? => out.push(u),
            _ => {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    resource,
                    "assignees",
                )));
            }
        }
    }
    Ok(out)
}

fn created_response(url: String, body: impl serde::Serialize) -> ApiResult<Response> {
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&url) {
        headers.insert(header::LOCATION, v);
    }
    Ok((StatusCode::CREATED, headers, Json(body)).into_response())
}

// ---------------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct CreateIssue {
    pub title: Option<Value>,
    pub body: Option<String>,
    pub assignee: Option<String>,
    pub assignees: Option<Vec<String>>,
    pub milestone: Option<Value>,
    pub labels: Option<Vec<Value>>,
}

/// `POST /repos/{owner}/{repo}/issues`
pub async fn create(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<CreateIssue>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    require_issues_enabled(&access)?;
    access.require_not_archived()?;
    let title = validate_title(body.title.as_ref())?;
    let triage = access.permission >= Permission::Triage;
    // Only triagers may set metadata; it's silently dropped otherwise.
    let (milestone, assignees, label_names) = if triage {
        let milestone = match &body.milestone {
            Some(v) => resolve_milestone(&state.db, access.repo.id, v).await?,
            None => None,
        };
        let mut logins = body.assignees.clone().unwrap_or_default();
        if let Some(a) = &body.assignee {
            logins.insert(0, a.clone());
        }
        let assignees = resolve_assignees(&state, &access.repo, &logins, "Issue").await?;
        let names = label_names(body.labels.as_deref().unwrap_or(&[]))?;
        (milestone, assignees, names)
    } else {
        (None, vec![], vec![])
    };
    let info = RepoInfo::from_access(&access);
    let mut tx = Tx::begin(&state).await?;
    let number = service::allocate_number(&mut tx, access.repo.id).await?;
    let issue: db::Issue = sqlx::query_as(&format!(
        "INSERT INTO issues (repo_id, number, title, body, author_id, milestone_id)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {}",
        db::Issue::COLUMNS
    ))
    .bind(access.repo.id)
    .bind(number)
    .bind(&title)
    .bind(body.body.as_deref())
    .bind(auth.user.id)
    .bind(milestone.as_ref().map(|m| m.id))
    .fetch_one(&mut *tx)
    .await?;
    if let Some(m) = &milestone {
        service::add_event(
            &mut tx,
            &issue,
            Some(auth.user.id),
            "milestoned",
            None,
            json!({ "milestone": { "title": m.title } }),
        )
        .await?;
        service::refresh_milestones(&mut tx, &[m.id]).await?;
    }
    let labels = service::resolve_labels(&mut tx, access.repo.id, &label_names, true).await?;
    service::add_labels(&mut tx, &issue, auth.user.id, &labels).await?;
    let ids: Vec<i64> = assignees.iter().map(|u| u.id).collect();
    service::add_assignees(&mut tx, &issue, auth.user.id, &ids).await?;
    service::subscribe(&mut tx, &issue, auth.user.id, "author").await?;
    if let Some(b) = issue.body.as_deref() {
        refs::process(&mut tx, &state, &info, &issue, None, None, b, &auth.user).await?;
    }
    let issue = service::touch_and_sync(&mut tx, issue.id, SyncAction::Insert).await?;
    service::sync_repo_open_issues(&mut tx, issue.repo_id).await?;
    tx.emit(Event::IssueOpened {
        repo_id: issue.repo_id,
        issue_id: issue.id,
        actor_id: auth.user.id,
    });
    tx.commit().await?;
    let rendered = json::issue(&state, fmt, &issue, &json::repo_map(&access)).await?;
    created_response(rendered.url.clone(), rendered)
}

// ---------------------------------------------------------------------------
// Get
// ---------------------------------------------------------------------------

/// `GET /repos/{owner}/{repo}/issues/{issue_number}` (301 for transferred
/// issues).
pub async fn get(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
) -> ApiResult<Response> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    let issue = match service::find_issue(&state.db, access.repo.id, number).await {
        Ok(i) => i,
        Err(ApiError::NotFound) => {
            return transferred_redirect(&state, auth.as_ref(), &access, number).await;
        }
        Err(e) => return Err(e),
    };
    if !issue.is_pull_request {
        require_issues_enabled(&access)?;
    }
    let rendered = json::issue(&state, fmt, &issue, &json::repo_map(&access)).await?;
    Ok(Json(rendered).into_response())
}

async fn transferred_redirect(
    state: &AppState,
    auth: Option<&AuthContext>,
    access: &RepoAccess,
    number: i64,
) -> ApiResult<Response> {
    let issue_id: Option<i64> = sqlx::query_scalar(
        "SELECT issue_id FROM issue_transfers WHERE old_repo_id = $1 AND old_number = $2",
    )
    .bind(access.repo.id)
    .bind(number)
    .fetch_optional(&state.db)
    .await?;
    let Some(issue_id) = issue_id else {
        return Err(ApiError::NotFound);
    };
    let issue = service::issue_by_id(&state.db, issue_id).await?;
    let repo = db::Repository::find(&state.db, issue.repo_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner = db::User::find(&state.db, repo.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Only reveal the new location to callers who can read it.
    let target = RepoAccess::for_repo(state, auth, repo, owner).await?;
    let url = state
        .urls
        .issue(&target.owner.login, &target.repo.name, issue.number);
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&url) {
        headers.insert(header::LOCATION, v);
    }
    Ok((
        StatusCode::MOVED_PERMANENTLY,
        headers,
        axum::Json(json!({
            "message": "Moved Permanently",
            "url": url,
            "documentation_url": "https://docs.github.com/rest/guides/best-practices-for-using-the-rest-api#follow-redirects",
        })),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct UpdateIssue {
    pub title: Option<Value>,
    #[serde(default, deserialize_with = "double")]
    pub body: Option<Option<String>>,
    pub state: Option<String>,
    #[serde(default, deserialize_with = "double")]
    pub state_reason: Option<Option<String>>,
    #[serde(default, deserialize_with = "double")]
    pub milestone: Option<Option<Value>>,
    pub labels: Option<Vec<Value>>,
    pub assignees: Option<Vec<String>>,
    #[serde(default, deserialize_with = "double")]
    pub assignee: Option<Option<String>>,
}

fn validate_state_reason(r: &str) -> ApiResult<()> {
    if matches!(r, "completed" | "not_planned" | "reopened" | "duplicate") {
        Ok(())
    } else {
        Err(ApiError::invalid_field(FieldError::invalid(
            "Issue",
            "state_reason",
        )))
    }
}

/// `PATCH /repos/{owner}/{repo}/issues/{issue_number}`
pub async fn update(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<UpdateIssue>,
) -> ApiResult<Json<json::Issue>> {
    let (access, issue) = load(&state, Some(&auth), &owner, &repo, number).await?;
    access.require_not_archived()?;
    let is_author = issue.author_id == Some(auth.user.id);
    let triage = access.permission >= Permission::Triage;
    let write = access.permission >= Permission::Write;
    let edits_content = body.title.is_some() || body.body.is_some();
    let edits_state = body.state.is_some() || body.state_reason.is_some();
    if (edits_content && !(is_author || write)) || (edits_state && !(is_author || triage)) {
        return Err(ApiError::forbidden(
            "You do not have permission to update this issue.",
        ));
    }
    if !is_author && !triage {
        return Err(ApiError::forbidden(
            "You do not have permission to update this issue.",
        ));
    }
    let title = match &body.title {
        Some(v) => Some(validate_title(Some(v))?),
        None => None,
    };
    if let Some(s) = &body.state
        && s != "open"
        && s != "closed"
    {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Issue", "state",
        )));
    }
    if let Some(Some(r)) = &body.state_reason {
        validate_state_reason(r)?;
    }
    let milestone = match (&body.milestone, triage) {
        (Some(Some(v)), true) => Some(resolve_milestone(&state.db, access.repo.id, v).await?),
        (Some(None), true) => Some(None),
        _ => None,
    };
    let assignees = if triage && (body.assignees.is_some() || body.assignee.is_some()) {
        let mut logins = body.assignees.clone().unwrap_or_default();
        if let Some(Some(a)) = &body.assignee {
            logins.insert(0, a.clone());
        }
        Some(resolve_assignees(&state, &access.repo, &logins, "Issue").await?)
    } else {
        None
    };
    let label_names = match (&body.labels, triage) {
        (Some(ls), true) => Some(label_names(ls)?),
        _ => None,
    };

    let info = RepoInfo::from_access(&access);
    let mut tx = Tx::begin(&state).await?;
    let issue = service::lock_issue(&mut tx, issue.id).await?;
    let mut changes = serde_json::Map::new();
    if let Some(t) = &title
        && *t != issue.title
    {
        sqlx::query("UPDATE issues SET title = $2 WHERE id = $1")
            .bind(issue.id)
            .bind(t)
            .execute(&mut *tx)
            .await?;
        service::add_event(
            &mut tx,
            &issue,
            Some(auth.user.id),
            "renamed",
            None,
            json!({ "rename": { "from": issue.title, "to": t } }),
        )
        .await?;
        changes.insert("title".into(), json!({ "from": issue.title }));
    }
    if let Some(b) = &body.body
        && *b != issue.body
    {
        sqlx::query("UPDATE issues SET body = $2 WHERE id = $1")
            .bind(issue.id)
            .bind(b.as_deref())
            .execute(&mut *tx)
            .await?;
        changes.insert("body".into(), json!({ "from": issue.body }));
        if let Some(text) = b.as_deref() {
            refs::process(
                &mut tx,
                &state,
                &info,
                &issue,
                None,
                issue.body.as_deref(),
                text,
                &auth.user,
            )
            .await?;
        }
    }
    if let Some(ms) = &milestone {
        service::set_milestone(&mut tx, &issue, auth.user.id, ms.as_ref()).await?;
    }
    if let Some(names) = &label_names {
        let labels = service::resolve_labels(&mut tx, access.repo.id, names, true).await?;
        service::replace_labels(&mut tx, &issue, auth.user.id, &labels).await?;
    }
    if let Some(users) = &assignees {
        let ids: Vec<i64> = users.iter().map(|u| u.id).collect();
        service::replace_assignees(&mut tx, &issue, auth.user.id, &ids).await?;
    }
    // State last: milestone counts then reflect the final milestone.
    let current = service::issue_by_id(&mut *tx, issue.id).await?;
    let reason = body.state_reason.clone().flatten();
    match body.state.as_deref() {
        Some(s) => {
            let reason = if s == "open" { None } else { reason.as_deref() };
            service::set_state(&mut tx, &current, auth.user.id, s, reason, None).await?;
        }
        None if current.state == "closed" && reason.is_some() => {
            service::set_state(
                &mut tx,
                &current,
                auth.user.id,
                "closed",
                reason.as_deref(),
                None,
            )
            .await?;
        }
        None => {}
    }
    let issue = service::touch_and_sync_with(
        &mut tx,
        issue.id,
        SyncAction::Update,
        changes.contains_key("body"),
    )
    .await?;
    if !changes.is_empty() {
        tx.emit(Event::IssueEdited {
            repo_id: issue.repo_id,
            issue_id: issue.id,
            actor_id: auth.user.id,
            changes: Value::Object(changes),
        });
    }
    tx.commit().await?;
    Ok(Json(
        json::issue(&state, fmt, &issue, &json::repo_map(&access)).await?,
    ))
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub milestone: Option<String>,
    pub state: Option<String>,
    pub assignee: Option<String>,
    pub creator: Option<String>,
    pub mentioned: Option<String>,
    pub labels: Option<String>,
    pub sort: Option<String>,
    pub direction: Option<String>,
    pub since: Option<String>,
    /// Cross-repository lists: assigned | created | mentioned | subscribed | repos | all.
    pub filter: Option<String>,
}

/// Parse GitHub's `since` (ISO 8601 timestamp or date).
pub fn parse_since(s: Option<&str>) -> ApiResult<Option<DateTime<Utc>>> {
    let Some(s) = s.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Ok(Some(d.with_timezone(&Utc)));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(0, 0, 0).map(|t| t.and_utc()));
    }
    Err(ApiError::unprocessable(format!(
        "Invalid request.\n\n\"{s}\" is not a valid ISO 8601 timestamp for \"since\"."
    )))
}

/// Shared filters (state, labels, user filters, since) and ordering.
/// Returns `false` if a filter can't match (unknown user/milestone).
async fn push_filters(
    state: &AppState,
    qb: &mut QueryBuilder<'_, Postgres>,
    q: &ListQuery,
    repo_id: Option<i64>,
) -> ApiResult<bool> {
    match q.state.as_deref().unwrap_or("open") {
        "open" => {
            qb.push(" AND i.state = 'open'");
        }
        "closed" => {
            qb.push(" AND i.state = 'closed'");
        }
        "all" => {}
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Issue", "state",
            )));
        }
    }
    if let (Some(m), Some(repo_id)) = (q.milestone.as_deref(), repo_id) {
        match m {
            "*" => {
                qb.push(" AND i.milestone_id IS NOT NULL");
            }
            "none" => {
                qb.push(" AND i.milestone_id IS NULL");
            }
            n => {
                let Ok(n) = n.parse::<i64>() else {
                    return Ok(false);
                };
                qb.push(" AND i.milestone_id = (SELECT id FROM milestones WHERE repo_id = ")
                    .push_bind(repo_id)
                    .push(" AND number = ")
                    .push_bind(n)
                    .push(")");
            }
        }
    }
    let user_id = |login: &str| {
        let state = state.clone();
        let login = login.to_string();
        async move {
            Ok::<_, ApiError>(
                db::User::find_by_login(&state.db, &login)
                    .await?
                    .map(|u| u.id),
            )
        }
    };
    match q.assignee.as_deref() {
        None => {}
        Some("*") => {
            qb.push(" AND EXISTS (SELECT 1 FROM issue_assignees a WHERE a.issue_id = i.id)");
        }
        Some("none") => {
            qb.push(" AND NOT EXISTS (SELECT 1 FROM issue_assignees a WHERE a.issue_id = i.id)");
        }
        Some(login) => {
            let Some(uid) = user_id(login).await? else {
                return Ok(false);
            };
            qb.push(" AND EXISTS (SELECT 1 FROM issue_assignees a WHERE a.issue_id = i.id AND a.user_id = ")
                .push_bind(uid)
                .push(")");
        }
    }
    if let Some(login) = q.creator.as_deref().filter(|s| !s.is_empty()) {
        let Some(uid) = user_id(login).await? else {
            return Ok(false);
        };
        qb.push(" AND i.author_id = ").push_bind(uid);
    }
    if let Some(login) = q.mentioned.as_deref().filter(|s| !s.is_empty()) {
        let Some(uid) = user_id(login).await? else {
            return Ok(false);
        };
        qb.push(" AND EXISTS (SELECT 1 FROM issue_mentions mm WHERE mm.issue_id = i.id AND mm.user_id = ")
            .push_bind(uid)
            .push(")");
    }
    if let Some(labels) = q.labels.as_deref() {
        for name in labels.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            qb.push(
                " AND EXISTS (SELECT 1 FROM issue_labels il JOIN labels l ON l.id = il.label_id \
                 WHERE il.issue_id = i.id AND lower(l.name) = lower(",
            )
            .push_bind(name.to_string())
            .push("))");
        }
    }
    if let Some(since) = parse_since(q.since.as_deref())? {
        qb.push(" AND i.updated_at >= ").push_bind(since);
    }
    Ok(true)
}

fn push_order(qb: &mut QueryBuilder<'_, Postgres>, q: &ListQuery, p: &Pagination) -> ApiResult<()> {
    let col = match q.sort.as_deref().unwrap_or("created") {
        "created" => "i.created_at",
        "updated" => "i.updated_at",
        "comments" => "i.comments_count",
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Issue", "sort",
            )));
        }
    };
    let dir = match q.direction.as_deref().unwrap_or("desc") {
        "asc" => "ASC",
        "desc" => "DESC",
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Issue",
                "direction",
            )));
        }
    };
    qb.push(format!(" ORDER BY {col} {dir}, i.id {dir} LIMIT "))
        .push_bind(p.limit_plus_one())
        .push(" OFFSET ")
        .push_bind(p.offset());
    Ok(())
}

/// `GET /repos/{owner}/{repo}/issues`
pub async fn list_for_repo(
    State(state): State<AppState>,
    auth: MaybeUser,
    fmt: BodyFormat,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<json::Issue>> {
    let access = RepoAccess::load(&state, auth.as_ref(), &owner, &repo).await?;
    require_issues_enabled(&access)?;
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "SELECT {} FROM issues i WHERE i.repo_id = ",
        db::prefixed("i", db::Issue::COLUMNS)
    ));
    qb.push_bind(access.repo.id);
    if !push_filters(&state, &mut qb, &q, Some(access.repo.id)).await? {
        return Ok(p.page(vec![]));
    }
    push_order(&mut qb, &q, &p)?;
    let rows: Vec<db::Issue> = qb.build_query_as().fetch_all(&state.db).await?;
    let page = p.page(rows);
    let rendered = json::issues(
        &state,
        fmt,
        &page.items,
        &json::repo_map(&access),
        IssueOpts::default(),
    )
    .await?;
    Ok(Page {
        items: rendered,
        link: page.link,
    })
}

/// Which repositories a cross-repository list covers.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// `GET /issues`: owned, member, collaborator, org and team repositories.
    All,
    /// `GET /user/issues`: owned and collaborator repositories.
    Owned,
    /// `GET /orgs/{org}/issues`.
    Org(i64),
}

/// Push a subquery selecting the ids of repositories the user has an
/// explicit relationship with.
fn push_member_repos(qb: &mut QueryBuilder<'_, Postgres>, uid: i64, scope: Scope) {
    qb.push("(SELECT r.id FROM repositories r WHERE r.owner_id = ")
        .push_bind(uid)
        .push(" UNION SELECT c.repo_id FROM collaborators c WHERE c.user_id = ")
        .push_bind(uid);
    if !matches!(scope, Scope::Owned) {
        qb.push(
            " UNION SELECT r.id FROM repositories r \
               JOIN org_members m ON m.org_id = r.owner_id AND m.user_id = ",
        )
        .push_bind(uid)
        .push(
            " LEFT JOIN org_settings s ON s.org_id = r.owner_id \
              WHERE m.role = 'admin' OR coalesce(s.default_repository_permission, 'read') <> 'none' \
             UNION SELECT tr.repo_id FROM team_repos tr WHERE tr.team_id IN (\
               WITH RECURSIVE ut AS (\
                 SELECT t.id, t.parent_id FROM team_members tm JOIN teams t ON t.id = tm.team_id \
                  WHERE tm.user_id = ",
        )
        .push_bind(uid)
        .push(
            " UNION SELECT pt.id, pt.parent_id FROM teams pt JOIN ut ON pt.id = ut.parent_id) \
               SELECT id FROM ut)",
        );
    }
    qb.push(")");
}

async fn list_cross_repo(
    state: &AppState,
    auth: &AuthContext,
    fmt: BodyFormat,
    p: Pagination,
    q: ListQuery,
    scope: Scope,
) -> ApiResult<Page<json::Issue>> {
    let uid = auth.user.id;
    let filter = q.filter.as_deref().unwrap_or("assigned");
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "SELECT {} FROM issues i JOIN repositories rr ON rr.id = i.repo_id WHERE rr.has_issues",
        db::prefixed("i", db::Issue::COLUMNS)
    ));
    match filter {
        "assigned" => {
            qb.push(" AND EXISTS (SELECT 1 FROM issue_assignees a WHERE a.issue_id = i.id AND a.user_id = ")
                .push_bind(uid)
                .push(")");
        }
        "created" => {
            qb.push(" AND i.author_id = ").push_bind(uid);
        }
        "mentioned" => {
            qb.push(" AND EXISTS (SELECT 1 FROM issue_mentions mm WHERE mm.issue_id = i.id AND mm.user_id = ")
                .push_bind(uid)
                .push(")");
        }
        "subscribed" => {
            qb.push(
                " AND EXISTS (SELECT 1 FROM thread_subscriptions ts WHERE ts.subject_id = i.id \
                 AND ts.subject_type IN ('Issue', 'PullRequest') AND ts.subscribed AND NOT ts.ignored \
                 AND ts.user_id = ",
            )
            .push_bind(uid)
            .push(")");
        }
        "repos" | "all" => {}
        _ => {
            return Err(ApiError::invalid_field(FieldError::invalid(
                "Issue", "filter",
            )));
        }
    }
    // Visibility: repositories the user is related to; for user-centric
    // filters also public repositories.
    if matches!(filter, "repos" | "all") {
        qb.push(" AND i.repo_id IN ");
        push_member_repos(&mut qb, uid, scope);
    } else {
        qb.push(" AND (rr.visibility = 'public' OR i.repo_id IN ");
        push_member_repos(&mut qb, uid, scope);
        qb.push(")");
    }
    if !auth.has_scope("repo") {
        qb.push(" AND rr.visibility = 'public'");
    }
    if let Scope::Org(org_id) = scope {
        qb.push(" AND rr.owner_id = ").push_bind(org_id);
    }
    if !push_filters(state, &mut qb, &q, None).await? {
        return Ok(p.page(vec![]));
    }
    push_order(&mut qb, &q, &p)?;
    let rows: Vec<db::Issue> = qb.build_query_as().fetch_all(&state.db).await?;
    let page = p.page(rows);
    let repos = json::load_repos(state, page.items.iter().map(|i| i.repo_id)).await?;
    let mut rendered = json::issues(state, fmt, &page.items, &repos, IssueOpts::default()).await?;
    json::attach_repositories(state, Some(auth), &repos, &mut rendered, &page.items).await?;
    Ok(Page {
        items: rendered,
        link: page.link,
    })
}

/// `GET /issues`
pub async fn list_for_authenticated_user(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    p: Pagination,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<json::Issue>> {
    list_cross_repo(&state, &auth, fmt, p, q, Scope::All).await
}

/// `GET /user/issues`
pub async fn list_for_user_repos(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    p: Pagination,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<json::Issue>> {
    list_cross_repo(&state, &auth, fmt, p, q, Scope::Owned).await
}

/// `GET /orgs/{org}/issues`
pub async fn list_for_org(
    State(state): State<AppState>,
    auth: RequireUser,
    fmt: BodyFormat,
    p: Pagination,
    Path(org): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Page<json::Issue>> {
    let org = db::User::find_by_login(&state.db, &org)
        .await?
        .filter(db::User::is_org)
        .ok_or(ApiError::NotFound)?;
    list_cross_repo(&state, &auth, fmt, p, q, Scope::Org(org.id)).await
}
