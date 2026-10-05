//! Actions secrets at repository, organization and environment level
//! (GitHub's sealed-box `public-key` flow; see [`crate::crypto`]).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use bgh_core::pagination::Pagination;
use bgh_core::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{load_org, require_org_admin, valid_name, wrapped};
use crate::crypto::{self, BoxKeyPair};
use crate::json::secret_json;
use crate::models::SecretRow;

/// Owner of a secret / variable / key.
#[derive(Debug, Clone, Copy)]
pub enum Scope {
    Repo(i64),
    Org(i64),
    Env(i64),
}

impl Scope {
    pub fn column(self) -> &'static str {
        match self {
            Self::Repo(_) => "repo_id",
            Self::Org(_) => "org_id",
            Self::Env(_) => "environment_id",
        }
    }

    pub fn id(self) -> i64 {
        match self {
            Self::Repo(id) | Self::Org(id) | Self::Env(id) => id,
        }
    }
}

/// Repository scope for admins of the repo.
pub async fn repo_scope(
    state: &AppState,
    auth: &AuthContext,
    owner: &str,
    repo: &str,
) -> ApiResult<RepoAccess> {
    let access = RepoAccess::load(state, Some(auth), owner, repo).await?;
    access.require(Permission::Admin)?;
    Ok(access)
}

/// Environment of a repository (404 if missing).
pub async fn env_id(state: &AppState, access: &RepoAccess, name: &str) -> ApiResult<i64> {
    crate::scoped::environment_id(state, access.repo.id, Some(name))
        .await?
        .ok_or(ApiError::NotFound)
}

/// Key pair of a repository (environments use their repository's) or org,
/// created on first use.
pub async fn keypair(state: &AppState, scope: Scope) -> ApiResult<BoxKeyPair> {
    let (col, id) = match scope {
        Scope::Repo(id) => ("repo_id", id),
        Scope::Org(id) => ("org_id", id),
        Scope::Env(env) => {
            let repo: i64 =
                sqlx::query_scalar("SELECT repo_id FROM actions_environments WHERE id = $1")
                    .bind(env)
                    .fetch_one(&state.db)
                    .await?;
            ("repo_id", repo)
        }
    };
    let server = crypto::server_key(state)?;
    let select =
        format!("SELECT key_id, public_key, secret_key_enc FROM actions_keys WHERE {col} = $1");
    let row: Option<(String, String, Vec<u8>)> = sqlx::query_as(&select)
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    let row = match row {
        Some(r) => r,
        None => {
            let (key_id, public, secret_enc) = crypto::generate_keypair(&server);
            sqlx::query(&format!(
                "INSERT INTO actions_keys ({col}, key_id, public_key, secret_key_enc)
                 VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING"
            ))
            .bind(id)
            .bind(&key_id)
            .bind(&public)
            .bind(&secret_enc)
            .execute(&state.db)
            .await?;
            sqlx::query_as(&select)
                .bind(id)
                .fetch_one(&state.db)
                .await?
        }
    };
    BoxKeyPair::from_row(&server, row.0, row.1, &row.2).map_err(ApiError::internal)
}

pub async fn public_key_json(state: &AppState, scope: Scope) -> ApiResult<Json<Value>> {
    let k = keypair(state, scope).await?;
    Ok(Json(json!({"key_id": k.key_id, "key": k.public_key})))
}

fn selected_url(state: &AppState, org: &str, kind: &str, name: &str) -> String {
    state.urls.api(&format!(
        "/orgs/{org}/actions/{kind}/{}/repositories",
        bgh_core::urls::encode_segment(name)
    ))
}

async fn list_scope(
    state: &AppState,
    p: &Pagination,
    scope: Scope,
    org_login: Option<&str>,
) -> ApiResult<Response> {
    let col = scope.column();
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM actions_secrets WHERE {col} = $1"
    ))
    .bind(scope.id())
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<SecretRow> = sqlx::query_as(&format!(
        "SELECT {} FROM actions_secrets WHERE {col} = $1 ORDER BY upper(name) LIMIT $2 OFFSET $3",
        SecretRow::COLUMNS
    ))
    .bind(scope.id())
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|s| {
            secret_json(
                s,
                org_login.map(|o| selected_url(state, o, "secrets", &s.name)),
            )
        })
        .collect();
    Ok(wrapped(p, total, "secrets", items))
}

async fn get_scope(state: &AppState, scope: Scope, name: &str) -> ApiResult<SecretRow> {
    sqlx::query_as(&format!(
        "SELECT {} FROM actions_secrets WHERE {} = $1 AND upper(name) = upper($2)",
        SecretRow::COLUMNS,
        scope.column()
    ))
    .bind(scope.id())
    .bind(name)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct PutSecret {
    pub encrypted_value: Option<String>,
    pub key_id: Option<String>,
    pub visibility: Option<String>,
    pub selected_repository_ids: Option<Vec<i64>>,
}

/// Create or update; returns (created, secret id).
async fn put_scope(
    state: &AppState,
    scope: Scope,
    name: &str,
    body: &PutSecret,
) -> ApiResult<(bool, i64)> {
    if !valid_name(name) {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Secret", "name",
        )));
    }
    let encrypted = body.encrypted_value.as_deref().ok_or_else(|| {
        ApiError::invalid_field(FieldError::missing_field("Secret", "encrypted_value"))
    })?;
    let key_id = body
        .key_id
        .as_deref()
        .ok_or_else(|| ApiError::invalid_field(FieldError::missing_field("Secret", "key_id")))?;
    let pair = keypair(state, scope).await?;
    if key_id != pair.key_id {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Secret", "key_id",
        )));
    }
    let plain = pair
        .unseal_b64(encrypted)
        .ok_or_else(|| ApiError::invalid_field(FieldError::invalid("Secret", "encrypted_value")))?;
    if plain.len() > 48 * 1024 {
        return Err(ApiError::unprocessable(
            "Secret value is too large (max 48 KB).",
        ));
    }
    let value_enc = crypto::server_key(state)?.encrypt(&plain);
    let visibility = match scope {
        Scope::Org(_) => {
            let v = body.visibility.as_deref().unwrap_or("private");
            if !matches!(v, "all" | "private" | "selected") {
                return Err(ApiError::invalid_field(FieldError::invalid(
                    "Secret",
                    "visibility",
                )));
            }
            Some(v.to_string())
        }
        _ => None,
    };
    let col = scope.column();
    let existing: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT id FROM actions_secrets WHERE {col} = $1 AND upper(name) = upper($2)"
    ))
    .bind(scope.id())
    .bind(name)
    .fetch_optional(&state.db)
    .await?;
    let mut tx = state.db.begin().await?;
    let id: i64 = match existing {
        Some(id) => {
            sqlx::query(
                "UPDATE actions_secrets SET value_enc = $2, visibility = coalesce($3, visibility),
                        updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(&value_enc)
            .bind(&visibility)
            .execute(&mut *tx)
            .await?;
            id
        }
        None => sqlx::query_scalar(&format!(
            "INSERT INTO actions_secrets ({col}, name, value_enc, visibility)
             VALUES ($1, $2, $3, $4) RETURNING id"
        ))
        .bind(scope.id())
        .bind(name)
        .bind(&value_enc)
        .bind(&visibility)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match bgh_core::db::unique_violation(&e) {
            Some(_) => ApiError::conflict("Secret already exists"),
            None => e.into(),
        })?,
    };
    if let (Scope::Org(org), Some(ids)) = (scope, &body.selected_repository_ids) {
        set_links(&mut tx, "actions_secret_repos", "secret_id", id, org, ids).await?;
    }
    tx.commit().await?;
    Ok((existing.is_none(), id))
}

/// Replace the selected repositories of an org secret/variable (repos must
/// belong to the org).
pub async fn set_links(
    conn: &mut sqlx::PgConnection,
    table: &str,
    fk: &str,
    id: i64,
    org_id: i64,
    repo_ids: &[i64],
) -> ApiResult<()> {
    sqlx::query(&format!("DELETE FROM {table} WHERE {fk} = $1"))
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(&format!(
        "INSERT INTO {table} ({fk}, repo_id)
         SELECT $1, r.id FROM repositories r WHERE r.id = ANY($2) AND r.owner_id = $3
         ON CONFLICT DO NOTHING"
    ))
    .bind(id)
    .bind(repo_ids)
    .bind(org_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn delete_scope(state: &AppState, scope: Scope, name: &str) -> ApiResult<StatusCode> {
    let n = sqlx::query(&format!(
        "DELETE FROM actions_secrets WHERE {} = $1 AND upper(name) = upper($2)",
        scope.column()
    ))
    .bind(scope.id())
    .bind(name)
    .execute(&state.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

fn put_status(created: bool) -> StatusCode {
    if created {
        StatusCode::CREATED
    } else {
        StatusCode::NO_CONTENT
    }
}

// ----- repository ------------------------------------------------------------

pub async fn repo_list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    list_scope(&state, &p, Scope::Repo(a.repo.id), None).await
}

pub async fn repo_public_key(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let a = RepoAccess::load(&state, Some(&auth), &owner, &repo).await?;
    a.require(Permission::Write)?;
    public_key_json(&state, Scope::Repo(a.repo.id)).await
}

pub async fn repo_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let s = get_scope(&state, Scope::Repo(a.repo.id), &name).await?;
    Ok(Json(secret_json(&s, None)))
}

pub async fn repo_put(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
    Json(body): Json<PutSecret>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let (created, _) = put_scope(&state, Scope::Repo(a.repo.id), &name, &body).await?;
    Ok((put_status(created), Json(json!({}))))
}

pub async fn repo_delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, name)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    delete_scope(&state, Scope::Repo(a.repo.id), &name).await
}

/// `GET /repos/{o}/{r}/actions/organization-secrets`
pub async fn repo_org_secrets(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((owner, repo)): Path<(String, String)>,
) -> ApiResult<Response> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let sql = "FROM actions_secrets s WHERE s.org_id = $1
           AND (s.visibility = 'all' OR (s.visibility = 'private' AND $3)
                OR (s.visibility = 'selected' AND EXISTS (
                     SELECT 1 FROM actions_secret_repos l WHERE l.secret_id = s.id AND l.repo_id = $2)))";
    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) {sql}"))
        .bind(a.owner.id)
        .bind(a.repo.id)
        .bind(a.repo.is_private())
        .fetch_one(&state.db)
        .await?;
    let rows: Vec<SecretRow> = sqlx::query_as(&format!(
        "SELECT {} {sql} ORDER BY upper(s.name) LIMIT $4 OFFSET $5",
        bgh_core::models::db::prefixed("s", SecretRow::COLUMNS)
    ))
    .bind(a.owner.id)
    .bind(a.repo.id)
    .bind(a.repo.is_private())
    .bind(p.limit())
    .bind(p.offset())
    .fetch_all(&state.db)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|s| {
            let mut v = secret_json(s, None);
            if let Some(o) = v.as_object_mut() {
                o.remove("visibility");
                o.remove("selected_repositories_url");
            }
            v
        })
        .collect();
    Ok(wrapped(&p, total, "secrets", items))
}

// ----- environment -----------------------------------------------------------

pub async fn env_list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((owner, repo, env)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let e = env_id(&state, &a, &env).await?;
    list_scope(&state, &p, Scope::Env(e), None).await
}

pub async fn env_public_key(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, env)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let e = env_id(&state, &a, &env).await?;
    public_key_json(&state, Scope::Env(e)).await
}

pub async fn env_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, env, name)): Path<(String, String, String, String)>,
) -> ApiResult<Json<Value>> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let e = env_id(&state, &a, &env).await?;
    let s = get_scope(&state, Scope::Env(e), &name).await?;
    Ok(Json(secret_json(&s, None)))
}

pub async fn env_put(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, env, name)): Path<(String, String, String, String)>,
    Json(body): Json<PutSecret>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let e = env_id(&state, &a, &env).await?;
    let (created, _) = put_scope(&state, Scope::Env(e), &name, &body).await?;
    Ok((put_status(created), Json(json!({}))))
}

pub async fn env_delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((owner, repo, env, name)): Path<(String, String, String, String)>,
) -> ApiResult<StatusCode> {
    let a = repo_scope(&state, &auth, &owner, &repo).await?;
    let e = env_id(&state, &a, &env).await?;
    delete_scope(&state, Scope::Env(e), &name).await
}

// ----- organization ----------------------------------------------------------

pub async fn org_list(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path(org): Path<String>,
) -> ApiResult<Response> {
    let o = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &o).await?;
    list_scope(&state, &p, Scope::Org(o.id), Some(&o.login)).await
}

pub async fn org_public_key(
    State(state): State<AppState>,
    auth: RequireUser,
    Path(org): Path<String>,
) -> ApiResult<Json<Value>> {
    let o = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &o).await?;
    public_key_json(&state, Scope::Org(o.id)).await
}

pub async fn org_get(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let o = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &o).await?;
    let s = get_scope(&state, Scope::Org(o.id), &name).await?;
    Ok(Json(secret_json(
        &s,
        Some(selected_url(&state, &o.login, "secrets", &s.name)),
    )))
}

pub async fn org_put(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name)): Path<(String, String)>,
    Json(body): Json<PutSecret>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let o = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &o).await?;
    if body.visibility.is_none() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "Secret",
            "visibility",
        )));
    }
    let (created, _) = put_scope(&state, Scope::Org(o.id), &name, &body).await?;
    Ok((put_status(created), Json(json!({}))))
}

pub async fn org_delete(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let o = load_org(&state, &org).await?;
    require_org_admin(&state, &auth, &o).await?;
    delete_scope(&state, Scope::Org(o.id), &name).await
}

/// Selected-repositories subresource shared by org secrets and variables.
pub mod selected {
    use super::*;
    use bgh_core::views;

    pub struct Kind {
        pub table: &'static str,
        pub link: &'static str,
        pub fk: &'static str,
    }

    pub const SECRETS: Kind = Kind {
        table: "actions_secrets",
        link: "actions_secret_repos",
        fk: "secret_id",
    };
    pub const VARIABLES: Kind = Kind {
        table: "actions_variables",
        link: "actions_variable_repos",
        fk: "variable_id",
    };

    async fn find(
        state: &AppState,
        kind: &Kind,
        org: &db::User,
        name: &str,
    ) -> ApiResult<(i64, Option<String>)> {
        sqlx::query_as(&format!(
            "SELECT id, visibility FROM {} WHERE org_id = $1 AND upper(name) = upper($2)",
            kind.table
        ))
        .bind(org.id)
        .bind(name)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)
    }

    fn require_selected(vis: Option<&str>) -> ApiResult<()> {
        if vis == Some("selected") {
            Ok(())
        } else {
            Err(ApiError::conflict(
                "The visibility of this item is not set to 'selected'",
            ))
        }
    }

    pub async fn list(
        state: &AppState,
        auth: &AuthContext,
        kind: &Kind,
        org: &str,
        name: &str,
        p: &Pagination,
    ) -> ApiResult<Response> {
        let o = load_org(state, org).await?;
        require_org_admin(state, auth, &o).await?;
        let (id, vis) = find(state, kind, &o, name).await?;
        require_selected(vis.as_deref())?;
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {} WHERE {} = $1",
            kind.link, kind.fk
        ))
        .bind(id)
        .fetch_one(&state.db)
        .await?;
        let rows: Vec<db::Repository> = sqlx::query_as(&format!(
            "SELECT {} FROM repositories r JOIN {} l ON l.repo_id = r.id
              WHERE l.{} = $1 ORDER BY r.id LIMIT $2 OFFSET $3",
            db::prefixed("r", db::Repository::COLUMNS),
            kind.link,
            kind.fk
        ))
        .bind(id)
        .bind(p.limit())
        .bind(p.offset())
        .fetch_all(&state.db)
        .await?;
        let items = views::minimal_repos(state, Some(auth), rows).await?;
        Ok(wrapped(p, total, "repositories", items))
    }

    pub async fn set(
        state: &AppState,
        auth: &AuthContext,
        kind: &Kind,
        org: &str,
        name: &str,
        ids: &[i64],
    ) -> ApiResult<StatusCode> {
        let o = load_org(state, org).await?;
        require_org_admin(state, auth, &o).await?;
        let (id, vis) = find(state, kind, &o, name).await?;
        require_selected(vis.as_deref())?;
        let mut tx = state.db.begin().await?;
        set_links(&mut tx, kind.link, kind.fk, id, o.id, ids).await?;
        tx.commit().await?;
        Ok(StatusCode::NO_CONTENT)
    }

    pub async fn add_or_remove(
        state: &AppState,
        auth: &AuthContext,
        kind: &Kind,
        org: &str,
        name: &str,
        repo_id: i64,
        add: bool,
    ) -> ApiResult<StatusCode> {
        let o = load_org(state, org).await?;
        require_org_admin(state, auth, &o).await?;
        let (id, vis) = find(state, kind, &o, name).await?;
        require_selected(vis.as_deref())?;
        if add {
            let n = sqlx::query(&format!(
                "INSERT INTO {} ({}, repo_id)
                 SELECT $1, r.id FROM repositories r WHERE r.id = $2 AND r.owner_id = $3
                 ON CONFLICT DO NOTHING",
                kind.link, kind.fk
            ))
            .bind(id)
            .bind(repo_id)
            .bind(o.id)
            .execute(&state.db)
            .await?
            .rows_affected();
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM repositories WHERE id = $1 AND owner_id = $2)",
            )
            .bind(repo_id)
            .bind(o.id)
            .fetch_one(&state.db)
            .await?;
            if n == 0 && !exists {
                return Err(ApiError::conflict(
                    "Repository is not owned by this organization",
                ));
            }
        } else {
            sqlx::query(&format!(
                "DELETE FROM {} WHERE {} = $1 AND repo_id = $2",
                kind.link, kind.fk
            ))
            .bind(id)
            .bind(repo_id)
            .execute(&state.db)
            .await?;
        }
        Ok(StatusCode::NO_CONTENT)
    }
}

#[derive(Debug, Deserialize)]
pub struct SelectedBody {
    pub selected_repository_ids: Vec<i64>,
}

pub async fn org_repos(
    State(state): State<AppState>,
    auth: RequireUser,
    p: Pagination,
    Path((org, name)): Path<(String, String)>,
) -> ApiResult<Response> {
    selected::list(&state, &auth, &selected::SECRETS, &org, &name, &p).await
}

pub async fn org_set_repos(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name)): Path<(String, String)>,
    Json(body): Json<SelectedBody>,
) -> ApiResult<StatusCode> {
    selected::set(
        &state,
        &auth,
        &selected::SECRETS,
        &org,
        &name,
        &body.selected_repository_ids,
    )
    .await
}

pub async fn org_add_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name, repo_id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    selected::add_or_remove(
        &state,
        &auth,
        &selected::SECRETS,
        &org,
        &name,
        repo_id,
        true,
    )
    .await
}

pub async fn org_remove_repo(
    State(state): State<AppState>,
    auth: RequireUser,
    Path((org, name, repo_id)): Path<(String, String, i64)>,
) -> ApiResult<StatusCode> {
    selected::add_or_remove(
        &state,
        &auth,
        &selected::SECRETS,
        &org,
        &name,
        repo_id,
        false,
    )
    .await
}
