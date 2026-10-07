//! `GET /_bgh/sync/bootstrap` and `GET /_bgh/sync/partial`
//! (docs/SYNC_PROTOCOL.md §4, §6).

use std::collections::{BTreeMap, BTreeSet};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
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

/// Open a consistent read-only snapshot (`REPEATABLE READ`, no JIT).
async fn snapshot(state: &AppState) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    // Large scopes have high plan costs; LLVM JIT compilation of the wide
    // JSON projections costs far more (~0.6 s) than it saves.
    sqlx::query("SET LOCAL jit = off").execute(&mut *tx).await?;
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

/// Response encodings we produce ourselves for large sync documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Br,
    Gzip,
}

impl Encoding {
    /// Pick from `Accept-Encoding` (brotli preferred; `q=0` excluded).
    fn negotiate(headers: &HeaderMap) -> Option<Self> {
        let accept = headers.get(header::ACCEPT_ENCODING)?.to_str().ok()?;
        let accepted = |name: &str| {
            accept.split(',').any(|part| {
                let mut it = part.split(';');
                let coding = it.next().unwrap_or("").trim();
                let q_zero = it.any(|p| {
                    p.trim()
                        .strip_prefix("q=")
                        .and_then(|q| q.trim().parse::<f32>().ok())
                        == Some(0.0)
                });
                coding.eq_ignore_ascii_case(name) && !q_zero
            })
        };
        if accepted("br") {
            Some(Self::Br)
        } else if accepted("gzip") {
            Some(Self::Gzip)
        } else {
            None
        }
    }
}

/// Bodies smaller than this are left to the generic compression layer.
const SELF_COMPRESS_MIN: usize = 64 * 1024;

/// JSON response for a (possibly large) sync document. Large bodies are
/// compressed here with fast settings (brotli q2 / gzip level 1) on the
/// blocking pool: the generic compression layer's defaults cost ~50 ms on a
/// 4 MB bootstrap; these cost a fraction for a similar ratio on this
/// highly repetitive JSON.
async fn sync_response(headers: &HeaderMap, body: String) -> ApiResult<Response> {
    let encoding = Encoding::negotiate(headers).filter(|_| body.len() >= SELF_COMPRESS_MIN);
    let Some(encoding) = encoding else {
        return Ok(json_response(body));
    };
    let compressed = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        use std::io::Write;
        let out = Vec::with_capacity(body.len() / 8);
        match encoding {
            Encoding::Br => {
                let mut w = brotli::CompressorWriter::new(out, 64 * 1024, 2, 22);
                w.write_all(body.as_bytes())?;
                Ok(w.into_inner())
            }
            Encoding::Gzip => {
                let mut w = flate2::write::GzEncoder::new(out, flate2::Compression::fast());
                w.write_all(body.as_bytes())?;
                w.finish()
            }
        }
    })
    .await??;
    let mut resp = json_response(String::new());
    *resp.body_mut() = Body::from(compressed);
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static(match encoding {
            Encoding::Br => "br",
            Encoding::Gzip => "gzip",
        }),
    );
    h.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    Ok(resp)
}

pub async fn bootstrap(
    State(state): State<AppState>,
    auth: RequireUser,
    headers: HeaderMap,
    Query(q): Query<BootstrapQuery>,
) -> ApiResult<Response> {
    compact::ensure_scheduled(&state).await;
    // The watermark before the snapshot: every action <= it is committed,
    // so the snapshot reflects it (and maybe later ones, which replaying
    // re-applies idempotently; SYNC_PROTOCOL.md §2).
    let last_sync_id = delta::head(&state.db).await?;
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
    let body = build(&state, &mut tx, viewer, &access, last_sync_id).await?;
    tx.commit().await?;
    sync_response(&headers, body).await
}

/// Assemble the bootstrap document. Model arrays are aggregated in Postgres
/// and spliced in as text, so the server never materializes row values.
async fn build(
    state: &AppState,
    conn: &mut PgConnection,
    viewer: i64,
    access: &scopes::Access,
    last_sync_id: i64,
) -> ApiResult<String> {
    let repos = access.repo_ids();
    let orgs = access.org_ids();
    let none = Opts::default();
    let mut parts: Vec<(&str, String)> = Vec::new();

    // Issues dominate; large sets are split across helper connections that
    // share this snapshot, and run while this connection does the rest.
    let issue_ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM issues WHERE repo_id = ANY($1) ORDER BY id")
            .bind(&repos)
            .fetch_all(&mut *conn)
            .await?;
    let mut local_chunks: Vec<&[i64]> = Vec::new();
    let mut helpers = Vec::new();
    if issue_ids.len() >= PARALLEL_MIN_ISSUES {
        let snapshot: String = sqlx::query_scalar("SELECT pg_export_snapshot()")
            .fetch_one(&mut *conn)
            .await?;
        let size = issue_ids.len().div_ceil(PARALLEL_CHUNKS);
        for (n, chunk) in issue_ids.chunks(size).enumerate() {
            // Only borrow idle connections, so concurrent bootstraps can't
            // starve each other; otherwise this connection does the chunk.
            match (n > 0).then(|| state.db.try_acquire()).flatten() {
                Some(helper) => helpers.push(tokio::spawn(load_issue_chunk(
                    helper,
                    snapshot.clone(),
                    chunk.to_vec(),
                ))),
                None => local_chunks.push(chunk),
            }
        }
    } else {
        local_chunks.push(&issue_ids);
    }

    // Models owned by other crates (bgh_core::sync::ScopeProvider), e.g.
    // projects in org:/user: scopes.
    let mut provided: Vec<(&'static str, Vec<serde_json::Value>)> = Vec::new();
    let mut users = referenced_users(conn, viewer, &repos, &orgs).await?;
    for scope in &access.allowed {
        let scope = scope.to_string();
        if scope.starts_with("repo:") {
            continue;
        }
        let rows = bgh_core::sync::load_provided(conn, &scope, Some(viewer)).await?;
        users.extend(rows.user_ids);
        provided.extend(rows.models);
    }
    users.sort_unstable();
    users.dedup();
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
    for model in [Model::Label, Model::Milestone] {
        let (rows, _) = shapes::load_joined(conn, model, Filter::Repos(&repos), none).await?;
        parts.push((model.name(), rows));
    }
    let mut issues: Vec<String> = Vec::new();
    for chunk in local_chunks {
        issues.push(
            shapes::load_joined(conn, Model::Issue, Filter::Ids(chunk), none)
                .await?
                .0,
        );
    }
    for helper in helpers {
        issues.push(helper.await??);
    }
    issues.retain(|s| !s.is_empty());
    parts.push(("issue", issues.join(",")));
    if access.has_user_scope(viewer) {
        let (rows, _) =
            shapes::load_joined(conn, Model::Notification, Filter::Users(&[viewer]), none).await?;
        parts.push(("notification", rows));
    }

    let mut provided_names: Vec<&'static str> = Vec::new();
    for (m, _) in &provided {
        if !provided_names.contains(m) {
            provided_names.push(m);
        }
    }
    for name in provided_names {
        let rows: Vec<String> = provided
            .iter()
            .filter(|(m, _)| *m == name)
            .flat_map(|(_, rows)| rows.iter())
            .map(serde_json::to_string)
            .collect::<Result<_, _>>()?;
        parts.push((name, rows.join(",")));
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

/// Issue sets at least this large are loaded in parallel.
const PARALLEL_MIN_ISSUES: usize = 2000;
/// Number of parallel issue chunks (this connection + up to 3 helpers).
const PARALLEL_CHUNKS: usize = 4;

/// Load one issue chunk on a helper connection that imports the bootstrap
/// snapshot (`SET TRANSACTION SNAPSHOT`), so it sees exactly the same data.
async fn load_issue_chunk(
    mut conn: sqlx::pool::PoolConnection<Postgres>,
    snapshot: String,
    ids: Vec<i64>,
) -> Result<String, sqlx::Error> {
    if !snapshot.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(sqlx::Error::Protocol(format!("bad snapshot id {snapshot}")));
    }
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;
    // SET TRANSACTION SNAPSHOT takes no parameters (the id was validated).
    for sql in [
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY".to_string(),
        format!("SET TRANSACTION SNAPSHOT '{snapshot}'"),
        "SET LOCAL jit = off".to_string(),
    ] {
        sqlx::query(&sql).execute(&mut *tx).await?;
    }
    let (rows, _) =
        shapes::load_joined(&mut tx, Model::Issue, Filter::Ids(&ids), Opts::default()).await?;
    tx.commit().await?;
    Ok(rows)
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

    // Before the snapshot, as in `bootstrap`.
    let last_sync_id = delta::head(&state.db).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn neg(v: &str) -> Option<Encoding> {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_ENCODING, v.parse().unwrap());
        Encoding::negotiate(&h)
    }

    #[test]
    fn negotiates_encoding() {
        assert_eq!(neg("gzip, deflate, br, zstd"), Some(Encoding::Br));
        assert_eq!(neg("gzip"), Some(Encoding::Gzip));
        assert_eq!(neg("br;q=0, gzip;q=0.5"), Some(Encoding::Gzip));
        assert_eq!(neg("identity"), None);
        assert_eq!(Encoding::negotiate(&HeaderMap::new()), None);
    }
}
