//! Resolve logins and `owner/repo` names used in qualifiers to ids, with
//! GitHub's error when they don't exist or aren't visible.

use std::collections::HashMap;

use bgh_core::perms::ReadableRepos;
use bgh_core::prelude::*;

pub const NOT_SEARCHABLE: &str = "The listed users and repositories cannot be searched either \
    because the resources do not exist or you do not have permission to view them.";

pub fn not_searchable() -> ApiError {
    ApiError::invalid_field(FieldError {
        message: Some(NOT_SEARCHABLE.into()),
        ..FieldError::new("Search", "q", "invalid")
    })
}

/// Login → user id (lowercased keys). `@me` resolves to the caller.
#[derive(Debug, Default)]
pub struct Users(pub HashMap<String, i64>);

impl Users {
    pub async fn load(
        state: &AppState,
        auth: Option<&AuthContext>,
        logins: Vec<String>,
    ) -> ApiResult<Self> {
        let mut map = HashMap::new();
        let mut wanted: Vec<String> = Vec::new();
        for l in logins {
            if l == "@me" {
                match auth {
                    Some(a) => {
                        map.insert(l, a.user.id);
                    }
                    None => return Err(ApiError::requires_auth()),
                }
            } else {
                wanted.push(l.to_lowercase());
            }
        }
        wanted.sort();
        wanted.dedup();
        if !wanted.is_empty() {
            let rows: Vec<(i64, String)> =
                sqlx::query_as("SELECT id, lower(login) FROM users WHERE lower(login) = ANY($1)")
                    .bind(&wanted)
                    .fetch_all(&state.db)
                    .await?;
            for (id, login) in rows {
                map.insert(login, id);
            }
            if wanted.iter().any(|w| !map.contains_key(w)) {
                return Err(not_searchable());
            }
        }
        Ok(Self(map))
    }

    pub fn id(&self, login: &str) -> i64 {
        if login == "@me" {
            return self.0.get(login).copied().unwrap_or(0);
        }
        self.0.get(&login.to_lowercase()).copied().unwrap_or(0)
    }

    pub fn ids(&self, logins: &[String]) -> Vec<i64> {
        logins.iter().map(|l| self.id(l)).collect()
    }
}

/// `owner/name` → repository id, only for repositories the caller can read.
pub async fn repos(
    state: &AppState,
    readable: &ReadableRepos,
    names: Vec<String>,
) -> ApiResult<HashMap<String, i64>> {
    let mut owners = Vec::new();
    let mut repos = Vec::new();
    for n in &names {
        let (o, r) = n.split_once('/').ok_or_else(not_searchable)?;
        owners.push(o.to_lowercase());
        repos.push(r.to_lowercase());
    }
    if names.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories r JOIN users u ON u.id = r.owner_id
          WHERE (lower(u.login), lower(r.name)) IN (SELECT * FROM unnest($1::text[], $2::text[]))",
        db::prefixed("r", db::Repository::COLUMNS)
    ))
    .bind(&owners)
    .bind(&repos)
    .fetch_all(&state.db)
    .await?;
    let owner_logins =
        bgh_core::views::users_by_id(state, rows.iter().map(|r| Some(r.owner_id))).await?;
    let mut map = HashMap::new();
    for r in rows.iter().filter(|r| readable.can_read(r)) {
        if let Some(o) = owner_logins.get(&r.owner_id) {
            map.insert(
                format!("{}/{}", o.login.to_lowercase(), r.name.to_lowercase()),
                r.id,
            );
        }
    }
    for n in &names {
        if !map.contains_key(&n.to_lowercase()) {
            return Err(not_searchable());
        }
    }
    Ok(map)
}
