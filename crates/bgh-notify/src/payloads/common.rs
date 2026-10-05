//! Shared building blocks: repository context, users, author associations,
//! reactions and URL helpers that `bgh_core::urls::Urls` lacks.

use std::collections::HashMap;

use bgh_core::AppState;
use bgh_core::models::api::{self, RepositoryExtras};
use bgh_core::models::db;
use bgh_core::node_id::{self, NodeType};
use bgh_core::urls::Urls;
use bgh_core::views;
use serde_json::{Map, Value, json};

/// A repository with its owner, loaded once per event and shared by every
/// payload builder.
#[derive(Debug, Clone)]
pub struct RepoCtx {
    pub repo: db::Repository,
    pub owner: db::User,
    /// `org_settings.description` when the owner is an organization.
    pub org_description: Option<String>,
}

impl RepoCtx {
    /// Load a repository and its owner; `None` when either is gone.
    pub async fn load(state: &AppState, repo_id: i64) -> anyhow::Result<Option<Self>> {
        let Some(repo) = db::Repository::find(&state.db, repo_id).await? else {
            return Ok(None);
        };
        let Some(owner) = db::User::find(&state.db, repo.owner_id).await? else {
            return Ok(None);
        };
        let org_description = if owner.is_org() {
            org_description(state, owner.id).await?
        } else {
            None
        };
        Ok(Some(Self {
            repo,
            owner,
            org_description,
        }))
    }

    pub fn owner_login(&self) -> &str {
        &self.owner.login
    }

    pub fn name(&self) -> &str {
        &self.repo.name
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner.login, self.repo.name)
    }

    /// `{api}/repos/{owner}/{repo}`
    pub fn api_url(&self, urls: &Urls) -> String {
        urls.repo(&self.owner.login, &self.repo.name)
    }

    /// `{base}/{owner}/{repo}`
    pub fn html_url(&self, urls: &Urls) -> String {
        urls.repo_html(&self.owner.login, &self.repo.name)
    }

    /// Organization id for org hooks (owner is an organization).
    pub fn org_id(&self) -> Option<i64> {
        self.owner.is_org().then_some(self.owner.id)
    }

    /// Full `repository` object as embedded in webhook payloads.
    pub fn repository(&self, urls: &Urls) -> Value {
        let r = api::Repository::new(
            urls,
            &self.repo,
            &self.owner,
            None,
            RepositoryExtras {
                subscribers_count: self.repo.watchers_count,
                ..RepositoryExtras::default()
            },
        );
        serde_json::to_value(r).unwrap_or(Value::Null)
    }

    /// `organization` (organization-simple) when the owner is an org.
    pub fn organization(&self, urls: &Urls) -> Option<Value> {
        self.owner.is_org().then(|| {
            serde_json::to_value(api::OrganizationSimple::new(
                urls,
                &self.owner,
                self.org_description.as_deref(),
            ))
            .unwrap_or(Value::Null)
        })
    }
}

/// `org_settings.description` for an organization.
pub(crate) async fn org_description(
    state: &AppState,
    org_id: i64,
) -> anyhow::Result<Option<String>> {
    Ok(sqlx::query_scalar::<_, Option<String>>(
        "SELECT description FROM org_settings WHERE org_id = $1",
    )
    .bind(org_id)
    .fetch_optional(&state.db)
    .await?
    .flatten())
}

/// Load a repository and render its full webhook `repository` object.
pub async fn repository(state: &AppState, repo_id: i64) -> anyhow::Result<Option<Value>> {
    Ok(RepoCtx::load(state, repo_id)
        .await?
        .map(|c| c.repository(&state.urls)))
}

/// organization-simple for an organization id (`None` if it is not an org).
pub async fn organization(state: &AppState, org_id: i64) -> anyhow::Result<Option<Value>> {
    let Some(org) = db::User::find(&state.db, org_id).await? else {
        return Ok(None);
    };
    if !org.is_org() {
        return Ok(None);
    }
    let desc = org_description(state, org.id).await?;
    Ok(Some(serde_json::to_value(api::OrganizationSimple::new(
        &state.urls,
        &org,
        desc.as_deref(),
    ))?))
}

/// simple-user JSON for a user row.
pub fn user_json(urls: &Urls, u: &db::User) -> Value {
    serde_json::to_value(api::SimpleUser::new(urls, u)).unwrap_or(Value::Null)
}

/// simple-user JSON from a user map, ghost when missing.
pub fn user_or_ghost(urls: &Urls, users: &HashMap<i64, db::User>, id: Option<i64>) -> Value {
    serde_json::to_value(api::SimpleUser::or_ghost(
        urls,
        id.and_then(|id| users.get(&id)),
    ))
    .unwrap_or(Value::Null)
}

/// simple-user JSON from a user map, `null` when missing.
pub fn user_or_null(urls: &Urls, users: &HashMap<i64, db::User>, id: Option<i64>) -> Value {
    id.and_then(|id| users.get(&id))
        .map(|u| user_json(urls, u))
        .unwrap_or(Value::Null)
}

/// `sender`: simple-user of the actor, the ghost user if unknown/deleted.
pub async fn sender(state: &AppState, user_id: Option<i64>) -> anyhow::Result<Value> {
    let user = match user_id {
        Some(id) => db::User::find(&state.db, id).await?,
        None => None,
    };
    Ok(serde_json::to_value(api::SimpleUser::or_ghost(
        &state.urls,
        user.as_ref(),
    ))?)
}

/// Batch-load users by id (one query).
pub(crate) async fn users(
    state: &AppState,
    ids: impl IntoIterator<Item = Option<i64>>,
) -> anyhow::Result<HashMap<i64, db::User>> {
    Ok(views::users_by_id(state, ids).await?)
}

/// GitHub's `author_association` precedence.
pub fn association(
    owner: bool,
    member: bool,
    collaborator: bool,
    contributor: bool,
) -> &'static str {
    if owner {
        "OWNER"
    } else if member {
        "MEMBER"
    } else if collaborator {
        "COLLABORATOR"
    } else if contributor {
        "CONTRIBUTOR"
    } else {
        "NONE"
    }
}

/// `author_association` for a set of users in a repository (one query).
pub async fn author_associations(
    state: &AppState,
    repo: &db::Repository,
    user_ids: &[i64],
) -> anyhow::Result<HashMap<i64, &'static str>> {
    let mut ids: Vec<i64> = user_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(i64, bool, bool, bool)> = sqlx::query_as(
        "SELECT u.id,
                EXISTS (SELECT 1 FROM org_members m
                        WHERE m.org_id = $1 AND m.user_id = u.id) AS member,
                EXISTS (SELECT 1 FROM collaborators c
                        WHERE c.repo_id = $2 AND c.user_id = u.id) AS collaborator,
                EXISTS (SELECT 1 FROM issues i JOIN pull_requests p ON p.issue_id = i.id
                        WHERE i.repo_id = $2 AND i.author_id = u.id AND p.merged) AS contributor
         FROM unnest($3::bigint[]) AS u(id)",
    )
    .bind(repo.owner_id)
    .bind(repo.id)
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, member, collab, contrib)| {
            (
                id,
                association(id == repo.owner_id, member, collab, contrib),
            )
        })
        .collect())
}

/// Look up an association, `NONE` for unknown/deleted users.
pub(crate) fn assoc_of(map: &HashMap<i64, &'static str>, id: Option<i64>) -> &'static str {
    id.and_then(|id| map.get(&id).copied()).unwrap_or("NONE")
}

/// `reaction-rollup` for a subject (one GROUP BY query).
pub async fn reactions(
    state: &AppState,
    subject_type: &str,
    subject_id: i64,
    url: String,
) -> anyhow::Result<Value> {
    let counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT content, count(*) FROM reactions
         WHERE subject_type = $1 AND subject_id = $2 GROUP BY content",
    )
    .bind(subject_type)
    .bind(subject_id)
    .fetch_all(&state.db)
    .await?;
    Ok(serde_json::to_value(api::ReactionRollup::from_counts(
        url, &counts,
    ))?)
}

/// Users with a verified email matching one of `emails` (lowercased keys),
/// one query.
pub(crate) async fn users_by_email(
    state: &AppState,
    emails: &[String],
) -> anyhow::Result<HashMap<String, db::User>> {
    let mut keys: Vec<String> = emails.iter().map(|e| e.to_lowercase()).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.retain(|e| !e.is_empty());
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        email_key: String,
        #[sqlx(flatten)]
        user: db::User,
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT lower(e.email) AS email_key, {} FROM user_emails e
         JOIN users u ON u.id = e.user_id
         WHERE e.verified AND lower(e.email) = ANY($1)",
        db::prefixed("u", db::User::COLUMNS)
    ))
    .bind(&keys)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(|r| (r.email_key, r.user)).collect())
}

/// `node_id` for a commit (`"{repo_id}:{sha}"` key).
pub fn commit_node_id(repo_id: i64, sha: &str) -> String {
    node_id::encode_str(NodeType::Commit, &format!("{repo_id}:{sha}"))
}

/// Assemble a payload: `action` (if any), the given entries, then
/// `repository`, `organization` (org-owned repos only) and `sender`.
pub(crate) fn envelope(
    urls: &Urls,
    ctx: &RepoCtx,
    action: Option<&str>,
    entries: Vec<(&str, Value)>,
    sender: Value,
) -> Value {
    let mut m = Map::new();
    if let Some(a) = action {
        m.insert("action".into(), json!(a));
    }
    for (k, v) in entries {
        m.insert(k.to_string(), v);
    }
    m.insert("repository".into(), ctx.repository(urls));
    if let Some(org) = ctx.organization(urls) {
        m.insert("organization".into(), org);
    }
    m.insert("sender".into(), sender);
    Value::Object(m)
}

/// `{"href": url}`
pub(crate) fn href(url: impl Into<String>) -> Value {
    json!({ "href": url.into() })
}

/// First `n` characters of a SHA (or the whole string if shorter).
pub(crate) fn short_sha(sha: &str, n: usize) -> &str {
    sha.get(..n).unwrap_or(sha)
}

/// Minimal glob (`*`, `?`) used for branch protection patterns.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn association_precedence() {
        assert_eq!(association(true, true, true, true), "OWNER");
        assert_eq!(association(false, true, true, false), "MEMBER");
        assert_eq!(association(false, false, true, true), "COLLABORATOR");
        assert_eq!(association(false, false, false, true), "CONTRIBUTOR");
        assert_eq!(association(false, false, false, false), "NONE");
    }

    #[test]
    fn globs() {
        assert!(glob_match("main", "main"));
        assert!(glob_match("release/*", "release/1.0"));
        assert!(glob_match("v?", "v1"));
        assert!(!glob_match("release/*", "main"));
        assert_eq!(short_sha("abcdef0123456789", 12), "abcdef012345");
        assert_eq!(short_sha("abc", 12), "abc");
    }
}
