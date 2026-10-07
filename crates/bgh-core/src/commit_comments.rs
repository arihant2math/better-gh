//! Commit comments (`commit_comments`, migration 4400): row type and
//! GitHub's REST shape, shared by the REST API (bgh-repos) and the
//! `commit_comment` webhook / `CommitCommentEvent` payloads (bgh-notify,
//! bgh-search).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::error::ApiResult;
use crate::markdown::{self, RenderContext};
use crate::models::api::{ReactionRollup, SimpleUser};
use crate::models::db;
use crate::node_id::{self, NodeType};
use crate::state::AppState;
use crate::time::Timestamp;

/// `reactions.subject_type` of commit comments.
pub const REACTION_SUBJECT: &str = "commit_comment";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CommitCommentRow {
    pub id: i64,
    pub repo_id: i64,
    pub commit_id: String,
    pub path: Option<String>,
    pub position: Option<i32>,
    pub line: Option<i32>,
    pub body: String,
    pub user_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl CommitCommentRow {
    pub const COLUMNS: &'static str =
        "id, repo_id, commit_id, path, position, line, body, user_id, created_at, updated_at";

    pub async fn find(
        db: impl sqlx::PgExecutor<'_>,
        repo_id: i64,
        id: i64,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM commit_comments WHERE id = $1 AND repo_id = $2",
            Self::COLUMNS
        ))
        .bind(id)
        .bind(repo_id)
        .fetch_optional(db)
        .await
    }
}

/// Which body representations to include (`Accept:
/// application/vnd.github.{raw,text,html,full}+json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyFormat {
    pub body: bool,
    pub text: bool,
    pub html: bool,
}

impl Default for BodyFormat {
    fn default() -> Self {
        Self::RAW
    }
}

impl BodyFormat {
    pub const RAW: Self = Self {
        body: true,
        text: false,
        html: false,
    };
    pub const FULL: Self = Self {
        body: true,
        text: true,
        html: true,
    };
}

/// `html_url` of a commit comment.
pub fn html_url(state: &AppState, owner: &str, repo: &str, c: &CommitCommentRow) -> String {
    format!(
        "{}#commitcomment-{}",
        state.urls.commit_html(owner, repo, &c.commit_id),
        c.id
    )
}

/// GitHub's `commit-comment` objects for `rows` of one repository, with
/// users, `author_association` and reaction rollups batch-loaded (three
/// queries regardless of the number of rows).
pub async fn render(
    state: &AppState,
    owner: &str,
    repo: &db::Repository,
    rows: &[CommitCommentRow],
    fmt: BodyFormat,
) -> ApiResult<Vec<Value>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let users = crate::views::users_by_id(state, rows.iter().map(|r| r.user_id)).await?;
    let assoc = associations(state, repo, rows.iter().filter_map(|r| r.user_id)).await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let counts: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT subject_id, content, count(*) FROM reactions
          WHERE subject_type = 'commit_comment' AND subject_id = ANY($1)
          GROUP BY subject_id, content",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let mut by_comment: HashMap<i64, Vec<(String, i64)>> = HashMap::new();
    for (id, content, n) in counts {
        by_comment.entry(id).or_default().push((content, n));
    }
    let autolinks = if fmt.html || fmt.text {
        markdown::repo_autolinks(&state.db, repo.id).await
    } else {
        Vec::new()
    };
    let urls = &state.urls;
    let name = repo.name.as_str();
    Ok(rows
        .iter()
        .map(|c| {
            let url = urls.api(&format!("/repos/{owner}/{name}/comments/{}", c.id));
            let reactions = ReactionRollup::from_counts(
                format!("{url}/reactions"),
                by_comment.get(&c.id).map(Vec::as_slice).unwrap_or(&[]),
            );
            let mut v = json!({
                "html_url": html_url(state, owner, name, c),
                "url": url,
                "id": c.id,
                "node_id": node_id::encode(NodeType::CommitComment, c.id),
                "path": c.path,
                "position": c.position,
                "line": c.line,
                "commit_id": c.commit_id,
                "user": SimpleUser::or_ghost(urls, c.user_id.and_then(|u| users.get(&u))),
                "created_at": Timestamp::from(c.created_at),
                "updated_at": Timestamp::from(c.updated_at),
                "author_association": c
                    .user_id
                    .and_then(|u| assoc.get(&u).copied())
                    .unwrap_or("NONE"),
                "reactions": reactions,
            });
            if fmt.body {
                v["body"] = json!(c.body);
            }
            if fmt.html || fmt.text {
                let html = markdown::render(
                    &c.body,
                    &RenderContext::new(&state.config.base_url)
                        .with_repo(owner, name)
                        .with_autolinks(&autolinks),
                );
                if fmt.text {
                    v["body_text"] = json!(html_to_text(&html));
                }
                if fmt.html {
                    v["body_html"] = json!(html);
                }
            }
            v
        })
        .collect())
}

/// `author_association` of `users` in `repo` (one query).
async fn associations(
    state: &AppState,
    repo: &db::Repository,
    users: impl Iterator<Item = i64>,
) -> ApiResult<HashMap<i64, &'static str>> {
    let mut ids: Vec<i64> = users.collect();
    ids.sort_unstable();
    ids.dedup();
    let rows: Vec<(i64, bool, bool, bool)> = sqlx::query_as(
        "SELECT u.id,
                EXISTS (SELECT 1 FROM org_members m
                         WHERE m.org_id = $1 AND m.user_id = u.id),
                (EXISTS (SELECT 1 FROM collaborators c
                          WHERE c.repo_id = $2 AND c.user_id = u.id)
                 OR EXISTS (SELECT 1 FROM team_repos tr
                              JOIN team_members tm ON tm.team_id = tr.team_id
                             WHERE tr.repo_id = $2 AND tm.user_id = u.id)),
                EXISTS (SELECT 1 FROM issues i JOIN pull_requests p ON p.issue_id = i.id
                         WHERE i.repo_id = $2 AND i.author_id = u.id AND p.merged)
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
            let a = if id == repo.owner_id {
                "OWNER"
            } else if member {
                "MEMBER"
            } else if collab {
                "COLLABORATOR"
            } else if contrib {
                "CONTRIBUTOR"
            } else {
                "NONE"
            };
            (id, a)
        })
        .collect())
}

/// Plain text of sanitized HTML: tags stripped, common entities decoded.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::html_to_text;

    #[test]
    fn strips_tags() {
        assert_eq!(
            html_to_text("<p>Hi <strong>there</strong> &amp; you</p>\n"),
            "Hi there & you"
        );
    }
}
