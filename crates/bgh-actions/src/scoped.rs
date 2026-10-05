//! Resolution of secrets and variables visible to a job: organization
//! (by visibility), then repository, then environment values; later levels
//! override earlier ones (GitHub precedence: environment > repository >
//! organization).

use bgh_core::AppState;
use bgh_core::models::db;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::crypto;

/// Environment row id of `name` in `repo_id`.
pub async fn environment_id(
    state: &AppState,
    repo_id: i64,
    name: Option<&str>,
) -> Result<Option<i64>, sqlx::Error> {
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    sqlx::query_scalar(
        "SELECT id FROM actions_environments WHERE repo_id = $1 AND lower(name) = lower($2)",
    )
    .bind(repo_id)
    .bind(name)
    .fetch_optional(&state.db)
    .await
}

#[derive(sqlx::FromRow)]
struct Scoped {
    name: String,
    value: Vec<u8>,
}

/// Visible rows ordered org → repo → environment.
const SCOPED_SQL: &str = "
    SELECT name, VALUE FROM (
        SELECT 0 AS lvl, s.name, s.VALUE, s.id FROM TABLE s
         WHERE s.org_id = $2
           AND (s.visibility = 'all'
                OR (s.visibility = 'private' AND $4)
                OR (s.visibility = 'selected' AND EXISTS (
                      SELECT 1 FROM LINK l WHERE l.FK = s.id AND l.repo_id = $1)))
        UNION ALL
        SELECT 1, s.name, s.VALUE, s.id FROM TABLE s WHERE s.repo_id = $1
        UNION ALL
        SELECT 2, s.name, s.VALUE, s.id FROM TABLE s WHERE s.environment_id = $3
    ) x ORDER BY lvl, id";

fn scoped_sql(table: &str, link: &str, fk: &str, value: &str) -> String {
    SCOPED_SQL
        .replace("TABLE", table)
        .replace("LINK", link)
        .replace("FK", fk)
        .replace("VALUE", value)
}

/// Decrypted secrets for a job (names upper-cased, like GitHub).
pub async fn secrets_for(
    state: &AppState,
    repo: &db::Repository,
    environment: Option<&str>,
) -> anyhow::Result<IndexMap<String, String>> {
    let env_id = environment_id(state, repo.id, environment).await?;
    let rows: Vec<Scoped> = sqlx::query_as(&scoped_sql(
        "actions_secrets",
        "actions_secret_repos",
        "secret_id",
        "value_enc",
    ))
    .bind(repo.id)
    .bind(repo.owner_id)
    .bind(env_id)
    .bind(repo.is_private())
    .fetch_all(&state.db)
    .await?;
    let key = crypto::server_key(state).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut out = IndexMap::new();
    for r in rows {
        match key.decrypt_string(&r.value) {
            Ok(v) => {
                out.insert(r.name.to_ascii_uppercase(), v);
            }
            Err(err) => tracing::warn!(?err, name = %r.name, "skipping undecryptable secret"),
        }
    }
    Ok(out)
}

#[derive(sqlx::FromRow)]
struct ScopedVar {
    name: String,
    value: String,
}

/// The `vars` context for a repository (and environment).
pub async fn vars_for(
    state: &AppState,
    repo: &db::Repository,
    environment: Option<&str>,
) -> anyhow::Result<Map<String, Value>> {
    let env_id = environment_id(state, repo.id, environment).await?;
    let rows: Vec<ScopedVar> = sqlx::query_as(&scoped_sql(
        "actions_variables",
        "actions_variable_repos",
        "variable_id",
        "value",
    ))
    .bind(repo.id)
    .bind(repo.owner_id)
    .bind(env_id)
    .bind(repo.is_private())
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.name.to_ascii_uppercase(), Value::String(r.value)))
        .collect())
}
