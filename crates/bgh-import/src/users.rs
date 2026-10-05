//! Source user → local account.
//!
//! In order: an earlier mapping (shared by every import from the same
//! source host), the source profile's public email matching a **verified**
//! local email, the import's login map, else a **mannequin**: a
//! placeholder account that cannot sign in (no password, no email, so no
//! session, token or reset is possible) and shows the source login.
//! Reclaiming a mannequin is P51.

use std::collections::HashMap;

use bgh_core::prelude::*;
use serde_json::Value;

use crate::client::{GitHub, HttpError};
use crate::row::{self, ImportRow};

pub struct Users {
    import_id: i64,
    /// GitLab user objects (`username`, profile `/users/{id}` with
    /// `public_email`).
    gitlab: bool,
    host: String,
    map: HashMap<String, String>,
    cache: HashMap<i64, Option<i64>>,
}

/// How a source user was resolved (for the log).
enum Via {
    Email,
    LoginMap,
    Mannequin,
}

impl Via {
    fn describe(&self) -> &'static str {
        match self {
            Via::Email => "verified email",
            Via::LoginMap => "login map",
            Via::Mannequin => "mannequin",
        }
    }
}

impl Users {
    pub fn new(row: &ImportRow) -> Self {
        Self {
            import_id: row.id,
            gitlab: row.is_gitlab(),
            host: row.source_host(),
            map: row
                .options()
                .user_map
                .into_iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v))
                .collect(),
            cache: HashMap::new(),
        }
    }

    /// Local user for a source user object (`{login, id, ...}`); `None`
    /// for `null` and GitHub's `ghost` (rendered as ghost here too).
    pub async fn resolve(
        &mut self,
        state: &AppState,
        gh: &GitHub,
        user: &Value,
    ) -> anyhow::Result<Option<i64>> {
        let login = user["login"].as_str().or(user["username"].as_str());
        let (Some(source_id), Some(login)) = (user["id"].as_i64(), login) else {
            return Ok(None);
        };
        if login.eq_ignore_ascii_case("ghost") {
            return Ok(None);
        }
        if let Some(hit) = self.cache.get(&source_id) {
            return Ok(*hit);
        }
        let local = self
            .resolve_uncached(state, gh, source_id, login, user)
            .await?;
        self.cache.insert(source_id, local);
        Ok(local)
    }

    /// Resolve several users (assignees).
    pub async fn resolve_all(
        &mut self,
        state: &AppState,
        gh: &GitHub,
        users: &Value,
    ) -> anyhow::Result<Vec<i64>> {
        let mut out = Vec::new();
        for u in users.as_array().into_iter().flatten() {
            if let Some(id) = self.resolve(state, gh, u).await?
                && !out.contains(&id)
            {
                out.push(id);
            }
        }
        Ok(out)
    }

    async fn resolve_uncached(
        &mut self,
        state: &AppState,
        gh: &GitHub,
        source_id: i64,
        login: &str,
        user: &Value,
    ) -> anyhow::Result<Option<i64>> {
        let existing: Option<(i64, bool)> = sqlx::query_as(
            "SELECT m.local_id, u.mannequin FROM import_mappings m JOIN users u ON u.id = m.local_id
              WHERE m.scope = $1 AND m.source_type = 'user' AND m.source_id = $2",
        )
        .bind(&self.host)
        .bind(source_id.to_string())
        .fetch_optional(&state.db)
        .await?;
        // A real account sticks; a mannequin is only the fallback: a match
        // found now (email added since, a login map) replaces it for this
        // and later imports. Moving what earlier imports attributed to the
        // mannequin is reclaim (P51).
        let mannequin = match existing {
            Some((id, false)) => return Ok(Some(id)),
            Some((id, true)) => Some(id),
            None => None,
        };

        // The list payloads carry no email; the profile has the public one.
        let profile_path = if self.gitlab {
            format!("/users/{source_id}")
        } else {
            format!("/users/{login}")
        };
        let profile = match gh.get(&profile_path).await {
            Ok((p, _)) => p,
            Err(e)
                if e.downcast_ref::<HttpError>()
                    .is_some_and(|h| h.status == 404) =>
            {
                Value::Null
            }
            Err(e) => return Err(e),
        };
        let mut via = None;
        let mut local: Option<i64> = None;
        let email_key = if self.gitlab { "public_email" } else { "email" };
        if let Some(email) = profile[email_key].as_str().filter(|e| !e.is_empty()) {
            local = sqlx::query_scalar(
                "SELECT e.user_id FROM user_emails e JOIN users u ON u.id = e.user_id
                  WHERE lower(e.email) = lower($1) AND e.verified AND u.type = 'User'
                    AND NOT u.mannequin",
            )
            .bind(email)
            .fetch_optional(&state.db)
            .await?;
            via = local.map(|_| Via::Email);
        }
        if local.is_none()
            && let Some(target) = self.map.get(&login.to_ascii_lowercase())
        {
            local = sqlx::query_scalar(
                "SELECT id FROM users WHERE lower(login) = lower($1) AND type = 'User' AND NOT mannequin",
            )
            .bind(target)
            .fetch_optional(&state.db)
            .await?;
            match local {
                Some(_) => via = Some(Via::LoginMap),
                None => {
                    row::log(
                        &state.db,
                        self.import_id,
                        "warn",
                        &format!("user map: no local user {target} for {login}"),
                    )
                    .await
                }
            }
        }

        if let Some(mannequin) = mannequin {
            let (Some(id), Some(via)) = (local, via) else {
                return Ok(Some(mannequin));
            };
            let mut tx = Tx::begin(state).await?;
            sqlx::query(
                "UPDATE import_mappings SET local_id = $3, import_id = $4
                  WHERE scope = $1 AND source_type = 'user' AND source_id = $2",
            )
            .bind(&self.host)
            .bind(source_id.to_string())
            .bind(id)
            .bind(self.import_id)
            .execute(&mut *tx)
            .await?;
            row::bump(&mut tx, self.import_id, "users_mapped", 1).await?;
            tx.commit().await?;
            row::log(
                &state.db,
                self.import_id,
                "info",
                &format!("user {login}: {} (replaces its mannequin)", via.describe()),
            )
            .await;
            return Ok(Some(id));
        }

        let mut tx = Tx::begin(state).await?;
        let (id, via) = match (local, via) {
            (Some(id), Some(via)) => (id, via),
            _ => {
                let avatar = profile["avatar_url"]
                    .as_str()
                    .or(user["avatar_url"].as_str());
                (
                    create_mannequin(&mut tx, &self.host, login, avatar).await?,
                    Via::Mannequin,
                )
            }
        };
        let inserted = sqlx::query(
            "INSERT INTO import_mappings (scope, source_type, source_id, local_id, import_id)
             VALUES ($1, 'user', $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(&self.host)
        .bind(source_id.to_string())
        .bind(id)
        .bind(self.import_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            // A concurrent import mapped this user first; use theirs.
            drop(tx);
            return Ok(sqlx::query_scalar(
                "SELECT local_id FROM import_mappings
                  WHERE scope = $1 AND source_type = 'user' AND source_id = $2",
            )
            .bind(&self.host)
            .bind(source_id.to_string())
            .fetch_optional(&state.db)
            .await?);
        }
        let key = match via {
            Via::Mannequin => "mannequins",
            _ => "users_mapped",
        };
        row::bump(&mut tx, self.import_id, key, 1).await?;
        tx.commit().await?;
        let how = via.describe();
        row::log(
            &state.db,
            self.import_id,
            "info",
            &format!("user {login}: {how}"),
        )
        .await;
        Ok(Some(id))
    }
}

/// Local login for a mannequin: the sanitized source login plus
/// `-imported` (never the bare source login, which a real person may want
/// to sign up with), made unique.
pub fn mannequin_base(login: &str) -> String {
    let mut s = String::new();
    for c in login.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-');
    let s = if s.is_empty() { "user" } else { s };
    let max = 39 - "-imported".len() - 3;
    format!("{}-imported", &s[..s.len().min(max)])
}

async fn create_mannequin(
    tx: &mut Tx,
    host: &str,
    login: &str,
    avatar: Option<&str>,
) -> ApiResult<i64> {
    let base = mannequin_base(login);
    for n in 1..1000 {
        let candidate = if n == 1 {
            base.clone()
        } else {
            format!("{base}{n}")
        };
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO users (login, type, name, avatar_url, mannequin, mannequin_source, mannequin_login)
             VALUES ($1, 'User', $2, $3, true, $4, $5)
             ON CONFLICT ((lower(login))) DO NOTHING RETURNING id",
        )
        .bind(&candidate)
        // Displayed name: the source login, like GitHub's mannequins.
        .bind(login)
        .bind(avatar)
        .bind(host)
        .bind(login)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(id) = id {
            return Ok(id);
        }
    }
    Err(ApiError::Internal(anyhow::anyhow!(
        "no free mannequin login for {login}"
    )))
}

#[cfg(test)]
mod tests {
    use super::mannequin_base;

    #[test]
    fn mannequin_logins() {
        assert_eq!(mannequin_base("octocat"), "octocat-imported");
        assert_eq!(mannequin_base("dependabot[bot]"), "dependabot-bot-imported");
        assert_eq!(mannequin_base("--"), "user-imported");
        assert!(mannequin_base(&"a".repeat(60)).len() <= 36);
    }
}
