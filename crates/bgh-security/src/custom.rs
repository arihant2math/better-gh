//! Custom secret patterns of repositories and organizations
//! (`/_bgh/{repos/{o}/{r},orgs/{org}}/secret-scanning/custom-patterns`;
//! GitHub has no public REST for them), the pattern dry run and the list
//! of built-in patterns.

use axum::extract::State;
use axum::http::StatusCode;
use bgh_core::audit::{self, Target};
use bgh_core::prelude::*;
use bgh_core::views;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::patterns::{self, Kind, Pattern};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PatternRow {
    pub id: i64,
    pub repo_id: Option<i64>,
    pub org_id: Option<i64>,
    pub name: String,
    pub pattern: String,
    pub test_string: Option<String>,
    pub push_protection: bool,
    pub created_by_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, repo_id, org_id, name, pattern, test_string, push_protection, \
    created_by_id, created_at, updated_at";

/// Compiled custom patterns applying to `repo_id` (its own and its
/// organization's). Invalid stored patterns are skipped.
pub async fn patterns_for_repo(
    db: impl sqlx::PgExecutor<'_>,
    repo_id: i64,
) -> Result<Vec<Pattern>, sqlx::Error> {
    let rows: Vec<PatternRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM secret_scanning_custom_patterns
          WHERE repo_id = $1
             OR org_id = (SELECT owner_id FROM repositories WHERE id = $1)
          ORDER BY id"
    ))
    .bind(repo_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| match patterns::compile_custom(&r.pattern) {
            Ok(regex) => Some(Pattern {
                secret_type: patterns::custom_secret_type(r.id),
                display_name: r.name,
                regex,
                kind: Kind::Custom(r.id),
                push_protected: r.push_protection,
            }),
            Err(e) => {
                tracing::warn!(pattern = r.id, "skipping invalid custom pattern: {e}");
                None
            }
        })
        .collect())
}

async fn render(state: &AppState, rows: Vec<PatternRow>) -> ApiResult<Vec<Value>> {
    let users = views::users_by_id(state, rows.iter().map(|r| r.created_by_id)).await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "name": r.name,
                "pattern": r.pattern,
                "secret_type": patterns::custom_secret_type(r.id),
                "test_string": r.test_string,
                "push_protection": r.push_protection,
                "scope": if r.repo_id.is_some() { "repository" } else { "organization" },
                "created_at": Timestamp(r.created_at),
                "updated_at": Timestamp(r.updated_at),
                "created_by": r.created_by_id
                    .map(|id| api::SimpleUser::or_ghost(&state.urls, users.get(&id))),
            })
        })
        .collect())
}

/// Whose patterns: a repository or an organization.
#[derive(Debug, Clone, Copy)]
enum Owner {
    Repo { id: i64, org_id: Option<i64> },
    Org(i64),
}

impl Owner {
    fn column(self) -> (&'static str, i64) {
        match self {
            Owner::Repo { id, .. } => ("repo_id", id),
            Owner::Org(id) => ("org_id", id),
        }
    }

    fn target(self) -> Target {
        match self {
            Owner::Repo { id, org_id } => Target::Repo { id, org_id },
            Owner::Org(id) => Target::Org(id),
        }
    }
}

async fn repo_owner(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
) -> ApiResult<Owner> {
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Admin)?;
    Ok(Owner::Repo {
        id: access.repo.id,
        org_id: access.owner.is_org().then_some(access.owner.id),
    })
}

async fn org_owner(state: &AppState, auth: &AuthContext, org: &str) -> ApiResult<Owner> {
    let org = db::User::find_by_login(&state.db, org)
        .await?
        .filter(|u| u.is_org())
        .ok_or(ApiError::NotFound)?;
    if !auth.user.site_admin {
        match bgh_core::perms::org_role(&state.db, org.id, auth.user.id).await? {
            Some(r) if r.is_admin() => auth.require_scope("admin:org")?,
            Some(_) => return Err(ApiError::forbidden("Must be an organization owner.")),
            None => return Err(ApiError::NotFound),
        }
    }
    Ok(Owner::Org(org.id))
}

async fn list(state: &AppState, owner: Owner) -> ApiResult<Json<Vec<Value>>> {
    let (col, id) = owner.column();
    let rows: Vec<PatternRow> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM secret_scanning_custom_patterns WHERE {col} = $1 ORDER BY id"
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(render(state, rows).await?))
}

#[derive(Debug, Default, Deserialize)]
pub struct PatternBody {
    pub name: Option<String>,
    pub pattern: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub test_string: Option<Option<String>>,
    pub push_protection: Option<bool>,
}

fn nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

fn field_error(field: &str, message: impl Into<String>) -> ApiError {
    ApiError::invalid_field(FieldError::custom("CustomPattern", field, message))
}

/// Validate a name / pattern / test string triple.
fn validate(name: &str, pattern: &str, test_string: Option<&str>) -> ApiResult<()> {
    if name.trim().is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "CustomPattern",
            "name",
        )));
    }
    if name.chars().count() > 100 {
        return Err(field_error(
            "name",
            "name is too long (maximum is 100 characters)",
        ));
    }
    let re = patterns::compile_custom(pattern).map_err(|e| field_error("pattern", e))?;
    if let Some(t) = test_string.filter(|t| !t.is_empty())
        && !re.is_match(t.as_bytes())
    {
        return Err(field_error(
            "test_string",
            "the pattern does not match the test string",
        ));
    }
    Ok(())
}

/// Queue a custom-pattern backfill of every repository the pattern
/// applies to that has secret scanning on.
async fn enqueue_backfills(state: &AppState, tx: &mut Tx, owner: Owner) -> ApiResult<()> {
    let site = bgh_core::settings::load(state).await?;
    if !site.secret_scanning.available {
        return Ok(());
    }
    let ids: Vec<i64> = match owner {
        Owner::Repo { id, .. } => vec![id],
        Owner::Org(org_id) => {
            sqlx::query_scalar("SELECT id FROM repositories WHERE owner_id = $1 ORDER BY id")
                .bind(org_id)
                .fetch_all(&mut **tx)
                .await?
        }
    };
    let enabled: Vec<i64> = sqlx::query_scalar(
        "SELECT r FROM unnest($1::bigint[]) AS r
          WHERE $2 OR EXISTS (SELECT 1 FROM repo_security_settings s
                               WHERE s.repo_id = r AND s.secret_scanning)",
    )
    .bind(&ids)
    .bind(site.secret_scanning.enable_all)
    .fetch_all(&mut **tx)
    .await?;
    for id in enabled {
        crate::jobs::enqueue_history_scan(tx, id, "custom_pattern_backfill").await?;
    }
    Ok(())
}

async fn create(
    state: &AppState,
    auth: &AuthContext,
    owner: Owner,
    body: PatternBody,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = body.name.unwrap_or_default().trim().to_string();
    let pattern = body.pattern.unwrap_or_default();
    let test_string = body.test_string.flatten().filter(|t| !t.is_empty());
    validate(&name, &pattern, test_string.as_deref())?;
    let (col, id) = owner.column();
    let mut tx = Tx::begin(state).await?;
    let row: PatternRow = sqlx::query_as(&format!(
        "INSERT INTO secret_scanning_custom_patterns
             ({col}, name, pattern, test_string, push_protection, created_by_id)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(&name)
    .bind(&pattern)
    .bind(&test_string)
    .bind(body.push_protection.unwrap_or(false))
    .bind(auth.user.id)
    .fetch_one(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "secret_scanning_custom_pattern.create",
        owner.target(),
        json!({"id": row.id, "name": name}),
    )
    .await?;
    enqueue_backfills(state, &mut tx, owner).await?;
    tx.commit().await?;
    let mut v = render(state, vec![row]).await?;
    Ok((StatusCode::CREATED, Json(v.remove(0))))
}

async fn find(state: &AppState, owner: Owner, pattern_id: i64) -> ApiResult<PatternRow> {
    let (col, id) = owner.column();
    sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM secret_scanning_custom_patterns WHERE id = $1 AND {col} = $2"
    ))
    .bind(pattern_id)
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn update(
    state: &AppState,
    auth: &AuthContext,
    owner: Owner,
    pattern_id: i64,
    body: PatternBody,
) -> ApiResult<Json<Value>> {
    let old = find(state, owner, pattern_id).await?;
    let name = body.name.map(|n| n.trim().to_string()).unwrap_or(old.name);
    let pattern = body.pattern.unwrap_or(old.pattern.clone());
    let test_string = match body.test_string {
        Some(t) => t.filter(|t| !t.is_empty()),
        None => old.test_string,
    };
    validate(&name, &pattern, test_string.as_deref())?;
    let mut tx = Tx::begin(state).await?;
    let row: PatternRow = sqlx::query_as(&format!(
        "UPDATE secret_scanning_custom_patterns
            SET name = $2, pattern = $3, test_string = $4, push_protection = $5, updated_at = now()
          WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(pattern_id)
    .bind(&name)
    .bind(&pattern)
    .bind(&test_string)
    .bind(body.push_protection.unwrap_or(old.push_protection))
    .fetch_one(&mut *tx)
    .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "secret_scanning_custom_pattern.update",
        owner.target(),
        json!({"id": row.id, "name": name}),
    )
    .await?;
    if pattern != old.pattern {
        enqueue_backfills(state, &mut tx, owner).await?;
    }
    tx.commit().await?;
    let mut v = render(state, vec![row]).await?;
    Ok(Json(v.remove(0)))
}

/// Deleting a pattern closes its open alerts (`pattern_deleted`).
async fn delete(
    state: &AppState,
    auth: &AuthContext,
    owner: Owner,
    pattern_id: i64,
) -> ApiResult<StatusCode> {
    let row = find(state, owner, pattern_id).await?;
    let mut tx = Tx::begin(state).await?;
    let closed: Vec<(i64, i64)> = sqlx::query_as(
        "UPDATE secret_scanning_alerts
            SET state = 'resolved', resolution = 'pattern_deleted', resolved_by_id = $2,
                resolved_at = now(), updated_at = now()
          WHERE custom_pattern_id = $1 AND state = 'open'
          RETURNING id, repo_id",
    )
    .bind(pattern_id)
    .bind(auth.user.id)
    .fetch_all(&mut *tx)
    .await?;
    for (alert_id, repo_id) in closed {
        tx.emit(Event::SecretScanningAlert {
            repo_id,
            alert_id,
            action: "resolved".into(),
            actor_id: Some(auth.user.id),
        });
    }
    sqlx::query("DELETE FROM secret_scanning_custom_patterns WHERE id = $1")
        .bind(pattern_id)
        .execute(&mut *tx)
        .await?;
    audit::log(
        &mut *tx,
        Some(&auth.user),
        "secret_scanning_custom_pattern.delete",
        owner.target(),
        json!({"id": row.id, "name": row.name}),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ----- routes ---------------------------------------------------------------

pub async fn list_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Vec<Value>>> {
    let o = repo_owner(&state, &auth, &owner, &repo).await?;
    list(&state, o).await
}

pub async fn create_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<PatternBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let o = repo_owner(&state, &auth, &owner, &repo).await?;
    create(&state, &auth, o, body).await
}

pub async fn update_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
    Json(body): Json<PatternBody>,
) -> ApiResult<Json<Value>> {
    let o = repo_owner(&state, &auth, &owner, &repo).await?;
    update(&state, &auth, o, id, body).await
}

pub async fn delete_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    let o = repo_owner(&state, &auth, &owner, &repo).await?;
    delete(&state, &auth, o, id).await
}

pub async fn list_org(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
) -> ApiResult<Json<Vec<Value>>> {
    let o = org_owner(&state, &auth, &org).await?;
    list(&state, o).await
}

pub async fn create_org(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
    Json(body): Json<PatternBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let o = org_owner(&state, &auth, &org).await?;
    create(&state, &auth, o, body).await
}

pub async fn update_org(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): Path<(String, i64)>,
    Json(body): Json<PatternBody>,
) -> ApiResult<Json<Value>> {
    let o = org_owner(&state, &auth, &org).await?;
    update(&state, &auth, o, id, body).await
}

pub async fn delete_org(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, id)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    let o = org_owner(&state, &auth, &org).await?;
    delete(&state, &auth, o, id).await
}

#[derive(Debug, Default, Deserialize)]
pub struct TestBody {
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub test_string: String,
}

/// `POST /_bgh/secret-scanning/custom-patterns/test`: compile `pattern` and
/// list its matches in `test_string` (character offsets).
pub async fn test_pattern(_auth: RequireUser, Json(body): Json<TestBody>) -> Json<Value> {
    match patterns::compile_custom(&body.pattern) {
        Err(e) => Json(json!({"valid": false, "error": e, "matches": []})),
        Ok(re) => {
            let text = body.test_string.as_bytes();
            let char_at = |byte: usize| String::from_utf8_lossy(&text[..byte]).chars().count();
            let matches: Vec<Value> = re
                .find_iter(text)
                .take(100)
                .map(|m| {
                    json!({
                        "start": char_at(m.start()),
                        "end": char_at(m.end()),
                        "text": String::from_utf8_lossy(m.as_bytes()),
                    })
                })
                .collect();
            Json(json!({"valid": true, "error": null, "matches": matches}))
        }
    }
}

/// `GET /_bgh/secret-scanning/patterns`: the built-in patterns.
pub async fn builtin(_auth: RequireUser) -> Json<Value> {
    Json(Value::Array(
        patterns::builtin_list()
            .into_iter()
            .map(|(ty, name, provider, push)| {
                json!({
                    "secret_type": ty,
                    "display_name": name,
                    "provider": provider,
                    "push_protected": push,
                })
            })
            .collect(),
    ))
}
