//! Commit identities: the default author for API writes, author/committer
//! request objects, and mapping commit emails back to accounts (batched).

use std::collections::HashMap;

use bgh_core::prelude::*;
use bgh_git::write::Identity;
use chrono::{DateTime, Utc};
use serde::Deserialize;

/// `{id}+{login}@users.noreply.{host}`
pub fn noreply_email(state: &AppState, user: &db::User) -> String {
    format!(
        "{}+{}@users.noreply.{}",
        user.id,
        user.login,
        state.config.hostname()
    )
}

/// The user's primary email (or noreply address) and display name.
pub async fn default_identity(state: &AppState, user: &db::User) -> ApiResult<Identity> {
    let email: Option<String> =
        sqlx::query_scalar("SELECT email FROM user_emails WHERE user_id = $1 AND is_primary")
            .bind(user.id)
            .fetch_optional(&state.db)
            .await?;
    Ok(Identity::new(
        user.name.clone().unwrap_or_else(|| user.login.clone()),
        email.unwrap_or_else(|| noreply_email(state, user)),
    ))
}

/// `author` / `committer` / `tagger` objects in request bodies.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IdentityInput {
    pub name: Option<String>,
    pub email: Option<String>,
    /// ISO-8601 timestamp.
    pub date: Option<String>,
}

impl IdentityInput {
    /// Validate into an [`Identity`]: `name` and `email` are required.
    pub fn to_identity(&self, resource: &str, field: &str) -> ApiResult<Identity> {
        let (Some(name), Some(email)) = (
            self.name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            self.email
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        ) else {
            return Err(ApiError::invalid_field(FieldError::custom(
                resource,
                field,
                format!("{field} must include name and email"),
            )));
        };
        if name.contains(['<', '>', '\n']) || email.contains(['<', '>', '\n']) {
            return Err(ApiError::invalid_field(FieldError::invalid(
                resource, field,
            )));
        }
        let when = match &self.date {
            Some(d) => Some(parse_date(d).ok_or_else(|| {
                ApiError::invalid_field(FieldError::invalid(resource, &format!("{field}.date")))
            })?),
            None => None,
        };
        Ok(Identity {
            name: name.to_string(),
            email: email.to_string(),
            when,
        })
    }
}

/// Parse an ISO-8601 timestamp (`2024-01-01T00:00:00Z`, offsets allowed,
/// or a date `2024-01-01`).
pub fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0).map(|t| t.and_utc());
    }
    None
}

#[derive(sqlx::FromRow)]
struct EmailUser {
    email: String,
    #[sqlx(flatten)]
    user: db::User,
}

/// Map commit emails (any case) to accounts with that verified email, or
/// whose noreply address it is. Keys are lowercased emails. Two queries at
/// most, whatever the number of emails.
pub async fn users_by_email<'a>(
    state: &AppState,
    emails: impl IntoIterator<Item = &'a str>,
) -> ApiResult<HashMap<String, db::User>> {
    let mut lower: Vec<String> = emails.into_iter().map(|e| e.to_ascii_lowercase()).collect();
    lower.sort();
    lower.dedup();
    let mut out = HashMap::new();
    if lower.is_empty() {
        return Ok(out);
    }
    let rows: Vec<EmailUser> = sqlx::query_as(&format!(
        "SELECT e.email, {} FROM user_emails e JOIN users u ON u.id = e.user_id
          WHERE lower(e.email) = ANY($1) AND e.verified",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(&lower)
    .fetch_all(&state.db)
    .await?;
    for r in rows {
        out.insert(r.email.to_ascii_lowercase(), r.user);
    }
    // noreply addresses: `{id}+{login}@users.noreply.{host}`
    let suffix = format!("@users.noreply.{}", state.config.hostname()).to_ascii_lowercase();
    let mut wanted: Vec<(String, i64)> = Vec::new();
    for e in &lower {
        if out.contains_key(e) {
            continue;
        }
        if let Some(local) = e.strip_suffix(&suffix)
            && let Some((id, _login)) = local.split_once('+')
            && let Ok(id) = id.parse::<i64>()
        {
            wanted.push((e.clone(), id));
        }
    }
    if !wanted.is_empty() {
        let ids: Vec<i64> = wanted.iter().map(|(_, id)| *id).collect();
        let users: HashMap<i64, db::User> = db::User::find_many(&state.db, &ids)
            .await?
            .into_iter()
            .map(|u| (u.id, u))
            .collect();
        for (email, id) in wanted {
            if let Some(u) = users.get(&id)
                && email.contains(&format!("+{}@", u.login.to_ascii_lowercase()))
            {
                out.insert(email, u.clone());
            }
        }
    }
    Ok(out)
}
