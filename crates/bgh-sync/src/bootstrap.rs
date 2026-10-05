//! `GET /_bgh/sync/bootstrap` and `GET /_bgh/sync/partial`
//! (docs/SYNC_PROTOCOL.md §4, §6).

use std::collections::{BTreeMap, BTreeSet};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bgh_core::prelude::*;
use bgh_core::sync::SCHEMA_VERSION;
use bgh_core::sync::shapes::{self, Filter, Model, Opts};
use serde::Deserialize;
use serde_json::Value;
use sqlx::{PgConnection, Postgres, Transaction};

use crate::compact;
use crate::delta;
use crate::scopes;

#[derive(Debug, Deserialize)]
pub struct BootstrapQuery {
    /// Comma separated scopes; absent = the viewer's default scope set.
    pub scopes: Option<String>,
}

/// Open a consistent read-only snapshot (`REPEATABLE READ`).
async fn snapshot(state: &AppState) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

fn json_response(body: String) -> Response {
    let mut resp = Body::from(body).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

pub async fn bootstrap(
    State(state): State<AppState>,
    auth: RequireUser,
    Query(q): Query<BootstrapQuery>,
) -> ApiResult<Response> {
    compact::ensure_scheduled(&state).await;
    let mut tx = snapshot(&state).await?;
    let viewer = auth.user.id;
    let requested: Vec<String> = match q.scopes.as_deref() {
        Some(s) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        None => scopes::default_scopes(&mut tx, viewer).await?,
    };
    let access = scopes::check(&mut tx, &auth, &requested).await?;
    let last_sync_id = delta::head(&mut *tx).await?;
    let body = build(&mut tx, viewer, &access, last_sync_id).await?;
    tx.commit().await?;
    Ok(json_response(body))
}

/// Assemble the bootstrap document. Model arrays are aggregated in Postgres
/// and spliced in as text, so the server never materializes row values.
async fn build(
    conn: &mut PgConnection,
    viewer: i64,
    access: &scopes::Access,
    last_sync_id: i64,
) -> ApiResult<String> {
    let repos = access.repo_ids();
    let orgs = access.org_ids();
    let none = Opts::default();
    let mut parts: Vec<(&str, String)> = Vec::new();

    let users = referenced_users(conn, viewer, &repos, &orgs).await?;
    parts.push((
        "user",
        shapes::load_joined(conn, Model::User, Filter::Ids(&users), none)
            .await?
            .0,
    ));
    parts.push((
        "org",
        shapes::load_joined(conn, Model::Org, Filter::Ids(&orgs), none)
            .await?
            .0,
    ));
    for model in [Model::Membership, Model::Team] {
        let (rows, _) = shapes::load_joined(conn, model, Filter::Orgs(&orgs), none).await?;
        parts.push((model.name(), rows));
    }
    parts.push((
        "repo",
        shapes::load_joined(conn, Model::Repo, Filter::Ids(&repos), none)
            .await?
            .0,
    ));
    let viewer_repos = shapes::viewer_repos(conn, viewer, &access.repo_perms).await?;
    let viewer_repos: Vec<String> = viewer_repos
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<_, _>>()?;
    parts.push(("viewerRepo", viewer_repos.join(",")));
    for model in [Model::Label, Model::Milestone, Model::Issue] {
        let (rows, _) = shapes::load_joined(conn, model, Filter::Repos(&repos), none).await?;
        parts.push((model.name(), rows));
    }
    if access.has_user_scope(viewer) {
        let (rows, _) =
            shapes::load_joined(conn, Model::Notification, Filter::Users(&[viewer]), none).await?;
        parts.push(("notification", rows));
    }

    let scopes: Vec<String> = access.allowed.iter().map(ToString::to_string).collect();
    let len: usize = parts.iter().map(|(_, s)| s.len() + 24).sum();
    let mut out = String::with_capacity(len + 256);
    out.push_str(&format!(
        "{{\"schemaVersion\":{SCHEMA_VERSION},\"lastSyncId\":{last_sync_id},\"userId\":{viewer},\"scopes\":{},\"denied\":{},\"models\":{{",
        serde_json::to_string(&scopes)?,
        serde_json::to_string(&access.denied)?,
    ));
    let mut first = true;
    for (name, rows) in parts {
        if rows.is_empty() {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        out.push('"');
        out.push_str(name);
        out.push_str("\":[");
        out.push_str(&rows);
        out.push(']');
    }
    out.push_str("}}");
    Ok(out)
}

/// Every user referenced by the rows of the bootstrap (plus the viewer).
async fn referenced_users(
    conn: &mut PgConnection,
    viewer: i64,
    repos: &[i64],
    orgs: &[i64],
) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT id FROM (
             SELECT $3::bigint AS id
             UNION SELECT author_id FROM issues WHERE repo_id = ANY($1)
             UNION SELECT a.user_id FROM issue_assignees a JOIN issues i ON i.id = a.issue_id
                    WHERE i.repo_id = ANY($1)
             UNION SELECT merged_by_id FROM pull_requests WHERE repo_id = ANY($1)
             UNION SELECT q.user_id FROM pr_requested_reviewers q
                     JOIN pull_requests p ON p.issue_id = q.pull_id WHERE p.repo_id = ANY($1)
             UNION SELECT user_id FROM org_members WHERE org_id = ANY($2)
             UNION SELECT tm.user_id FROM team_members tm JOIN teams t ON t.id = tm.team_id
                    WHERE t.org_id = ANY($2)
         ) u WHERE id IS NOT NULL ORDER BY id",
    )
    .bind(repos)
    .bind(orgs)
    .bind(viewer)
    .fetch_all(conn)
    .await
}

#[derive(Debug, Deserialize)]
pub struct PartialQuery {
    pub model: String,
    pub issue: Option<i64>,
    pub id: Option<i64>,
}

pub async fn partial(
    State(state): State<AppState>,
    auth: MaybeUser,
    Query(q): Query<PartialQuery>,
) -> ApiResult<Response> {
    let mut models = Vec::new();
    for name in q.model.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match Model::parse(name) {
            Some(m @ (Model::Issue | Model::Comment | Model::Review | Model::IssueEvent)) => {
                models.push(m)
            }
            _ => {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "Sync", "model",
                )));
            }
        }
    }
    let issue_only = models == [Model::Issue];
    let issue_id = if issue_only {
        q.id.or(q.issue)
    } else if models.contains(&Model::Issue) || models.is_empty() {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Sync", "model",
        )));
    } else {
        q.issue
    }
    .ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field(
            "Sync",
            if issue_only { "id" } else { "issue" },
        ))
    })?;

    let mut tx = snapshot(&state).await?;
    let repo_id: i64 = sqlx::query_scalar("SELECT repo_id FROM issues WHERE id = $1")
        .bind(issue_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
    let readable = match auth.as_ref() {
        Some(auth) => scopes::repo_permissions(&mut tx, auth, &[repo_id])
            .await?
            .contains_key(&repo_id),
        None => {
            sqlx::query_scalar("SELECT visibility = 'public' FROM repositories WHERE id = $1")
                .bind(repo_id)
                .fetch_one(&mut *tx)
                .await?
        }
    };
    if !readable {
        return Err(ApiError::NotFound);
    }
    let last_sync_id = delta::head(&mut *tx).await?;
    let opts = Opts {
        issue_body: true,
        viewer: auth.user_id(),
    };
    let mut out: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    let mut users = BTreeSet::new();
    let ids = [issue_id];
    let mut load = vec![Model::Issue];
    load.extend(models.iter().copied().filter(|m| *m != Model::Issue));
    for model in load {
        let filter = if model == Model::Issue {
            Filter::Ids(&ids)
        } else {
            Filter::Issues(&ids)
        };
        let rows = shapes::load(&mut tx, model, filter, opts).await?;
        for row in &rows {
            shapes::referenced_users(model.name(), &row.data, &mut users);
        }
        out.insert(model.name(), rows.into_iter().map(|r| r.data).collect());
    }
    let users: Vec<i64> = users.into_iter().collect();
    if !users.is_empty() {
        let rows = shapes::load(&mut tx, Model::User, Filter::Ids(&users), opts).await?;
        out.insert("user", rows.into_iter().map(|r| r.data).collect());
    }
    tx.commit().await?;
    let body = serde_json::json!({ "lastSyncId": last_sync_id, "models": out });
    Ok(json_response(serde_json::to_string(&body)?))
}
