//! `GET /_bgh/search?q=&limit=&repo=owner/name&org=login`: compact, prefix-matching
//! results for the web command palette (issues/PRs, repositories, users),
//! three index-backed queries run concurrently.

use std::time::Instant;

use axum::extract::State;
use bgh_core::perms;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Postgres, QueryBuilder};

use crate::query::{like_escape, tsquery_words};

/// Matches below which a query counts as rare (see the plan choice below).
const RARE_MATCHES: i64 = 500;

#[derive(Debug, Default, Deserialize)]
pub struct PaletteParams {
    pub q: Option<String>,
    pub limit: Option<i64>,
    /// Restrict issues to `owner/name`.
    pub repo: Option<String>,
    /// Restrict issues and repositories to those owned by this org/user
    /// login (ignored when `repo` is given).
    pub org: Option<String>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct IssueHit {
    pub id: i64,
    pub repo: String,
    pub number: i64,
    pub title: String,
    pub state: String,
    pub pull_request: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct RepoHit {
    pub id: i64,
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
    pub stargazers_count: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub struct UserHit {
    pub id: i64,
    pub login: String,
    pub name: Option<String>,
    #[serde(rename = "type")]
    #[sqlx(rename = "type")]
    pub kind: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PaletteResult {
    pub q: String,
    pub took_ms: f64,
    pub issues: Vec<IssueHit>,
    pub repos: Vec<RepoHit>,
    pub users: Vec<UserHit>,
}

/// `foo bar` → `foo:* & bar:*` (letters/digits only, so always valid).
pub fn prefix_tsquery(q: &str) -> Option<String> {
    let words = tsquery_words(q);
    if words.is_empty() {
        return None;
    }
    Some(
        words
            .iter()
            .map(|w| format!("{w}:*"))
            .collect::<Vec<_>>()
            .join(" & "),
    )
}

pub async fn search(
    State(state): State<AppState>,
    auth: MaybeUser,
    Query(params): Query<PaletteParams>,
) -> ApiResult<Json<PaletteResult>> {
    let started = Instant::now();
    let q = params.q.unwrap_or_default().trim().to_string();
    let limit = params.limit.unwrap_or(8).clamp(1, 50);
    if q.is_empty() {
        return Ok(Json(PaletteResult {
            q,
            took_ms: 0.0,
            issues: vec![],
            repos: vec![],
            users: vec![],
        }));
    }
    let readable = perms::readable_repos(&state.db, auth.as_ref()).await?;
    let (all, private_ids) = (readable.all, readable.private_ids.clone());

    let scope_repo: Option<i64> = match params.repo.as_deref().and_then(|r| r.split_once('/')) {
        Some((o, n)) => {
            let access = RepoAccess::load(&state, auth.as_ref(), o, n).await?;
            Some(access.repo.id)
        }
        None => None,
    };
    // Owner scope; an unknown login maps to 0 (no such id) → empty results.
    let scope_owner: Option<i64> = match params.org.as_deref().filter(|o| !o.is_empty()) {
        Some(login) if scope_repo.is_none() => Some(
            sqlx::query_scalar::<_, i64>("SELECT id FROM users WHERE lower(login) = lower($1)")
                .bind(login)
                .fetch_optional(&state.db)
                .await?
                .unwrap_or(0),
        ),
        _ => None,
    };
    let lower = q.to_lowercase();
    let number: Option<i64> = lower.trim_start_matches('#').parse().ok();
    let tsq = prefix_tsquery(&q);

    let issues = async {
        if tsq.is_none() && number.is_none() {
            return Ok::<_, sqlx::Error>(vec![]);
        }
        // Plan choice: walking `issues_updated_idx` finds the newest matches
        // instantly for common terms but scans everything for rare ones,
        // where a GIN bitmap + sort is cheap. A bounded GIN probe decides.
        let rare = match &tsq {
            Some(t) => {
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM (SELECT 1 FROM issues
                      WHERE search @@ to_tsquery('english', $1) LIMIT $2) x",
                )
                .bind(t)
                .bind(RARE_MATCHES)
                .fetch_one(&state.db)
                .await?
                    < RARE_MATCHES
            }
            None => true,
        };
        let order = if rare {
            "i.updated_at + interval '0 s'"
        } else {
            "i.updated_at"
        };
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "SELECT i.id, o.login || '/' || r.name AS repo, i.number, i.title, i.state,
                    i.is_pull_request AS pull_request, i.updated_at
               FROM issues i
               JOIN repositories r ON r.id = i.repo_id
               JOIN users o ON o.id = r.owner_id
              WHERE ",
        );
        if all {
            qb.push("TRUE");
        } else {
            qb.push("(r.visibility = 'public' OR r.id = ANY(")
                .push_bind(private_ids.clone())
                .push("))");
        }
        if let Some(repo_id) = scope_repo {
            qb.push(" AND i.repo_id = ").push_bind(repo_id);
        }
        if let Some(owner_id) = scope_owner {
            qb.push(" AND r.owner_id = ").push_bind(owner_id);
        }
        let by_number = number.filter(|_| scope_repo.is_some());
        qb.push(" AND (");
        match (&tsq, by_number) {
            (Some(t), Some(n)) => {
                qb.push("i.search @@ to_tsquery('english', ")
                    .push_bind(t.clone())
                    .push(") OR i.number = ")
                    .push_bind(n);
            }
            (Some(t), None) => {
                qb.push("i.search @@ to_tsquery('english', ")
                    .push_bind(t.clone())
                    .push(")");
            }
            (None, Some(n)) => {
                qb.push("i.number = ").push_bind(n);
            }
            (None, None) => {
                qb.push("FALSE");
            }
        }
        qb.push(") ORDER BY ");
        if let Some(n) = by_number {
            qb.push("(i.number = ").push_bind(n).push(") DESC, ");
        }
        qb.push(format!("{order} DESC, i.id DESC LIMIT "))
            .push_bind(limit);
        qb.build_query_as::<IssueHit>().fetch_all(&state.db).await
    };
    let like = format!("%{}%", like_escape(&lower));
    let prefix = format!("{}%", like_escape(&lower));
    let (owner_part, name_part) = match lower.split_once('/') {
        Some((o, n)) => (Some(o.to_string()), n.to_string()),
        None => (None, lower.clone()),
    };
    let name_like = format!("%{}%", like_escape(&name_part));
    let repos = async {
        sqlx::query_as::<_, RepoHit>(
            "SELECT r.id, o.login || '/' || r.name AS full_name, r.description,
                    r.visibility <> 'public' AS private, r.stargazers_count
               FROM repositories r JOIN users o ON o.id = r.owner_id
              WHERE ($1 OR r.visibility = 'public' OR r.id = ANY($2))
                AND lower(r.name) LIKE $3
                AND ($4::text IS NULL OR lower(o.login) = $4)
                AND ($7::bigint IS NULL OR r.owner_id = $7)
              ORDER BY (lower(r.name) = $5) DESC, r.stargazers_count DESC, r.id
              LIMIT $6",
        )
        .bind(all)
        .bind(&private_ids)
        .bind(&name_like)
        .bind(&owner_part)
        .bind(&name_part)
        .bind(limit)
        .bind(scope_owner)
        .fetch_all(&state.db)
        .await
    };
    let users = async {
        sqlx::query_as::<_, UserHit>(
            "SELECT id, login, name, type, avatar_url FROM users
              WHERE suspended_at IS NULL
                AND (lower(login) LIKE $1 OR lower(coalesce(name, '')) LIKE $2)
              ORDER BY (lower(login) = $3) DESC, (lower(login) LIKE $1) DESC, length(login), id
              LIMIT $4",
        )
        .bind(&prefix)
        .bind(&like)
        .bind(&lower)
        .bind(limit)
        .fetch_all(&state.db)
        .await
    };
    let (issues, repos, users) = tokio::try_join!(issues, repos, users)?;
    let users = users
        .into_iter()
        .map(|mut u| {
            u.avatar_url = Some(state.urls.avatar(u.id, u.avatar_url.as_deref()));
            u
        })
        .collect();
    Ok(Json(PaletteResult {
        q,
        took_ms: started.elapsed().as_secs_f64() * 1000.0,
        issues,
        repos,
        users,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_queries() {
        assert_eq!(
            prefix_tsquery("fix crash").as_deref(),
            Some("fix:* & crash:*")
        );
        assert_eq!(prefix_tsquery("'&|!").as_deref(), None);
    }
}
