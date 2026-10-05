//! `search(query:, type:)`: GitHub search syntax over issues, pull requests,
//! repositories and users.
//!
//! Supported qualifiers: `repo:`, `user:`/`org:`/`owner:`, `is:` (issue, pr,
//! open, closed, merged, unmerged, draft, public, private, archived),
//! `state:`, `author:`, `assignee:`, `mentions:`, `involves:`, `commenter:`,
//! `review-requested:`, `reviewed-by:`, `label:`, `no:` (label, assignee,
//! milestone), `milestone:`, `head:`, `base:`, `archived:`, `fork:`,
//! `in:` (title, body, name, description), `sort:`; `-` negates; `@me` is
//! the viewer; free text matches titles/bodies (names/descriptions for
//! repositories, logins/names for users).

use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::{Context, Object, SimpleObject, Union};
use bgh_core::perms::Permission;
use bgh_core::prelude::*;
use sqlx::{Postgres, QueryBuilder};

use crate::conn::{ConnArgs, Page, PageInfo, encode_cursor};
use crate::ctx::{GResult, OrGql, gql};
use crate::loaders::RepoLoader;
use crate::model::enums::SearchType;
use crate::model::pull::from_issues;
use crate::model::{Issue, Organization, PullRequest, Repository, User};

/// Upper bound on candidate rows considered per search (GitHub caps at 1000).
const MAX_RESULTS: i64 = 1000;

#[derive(Union, Clone)]
pub enum SearchResultItem {
    Issue(Issue),
    PullRequest(PullRequest),
    Repository(Repository),
    User(User),
    Organization(Organization),
}

#[derive(SimpleObject, Clone)]
pub struct SearchResultItemEdge {
    pub cursor: String,
    pub node: Option<SearchResultItem>,
    pub text_matches: Option<Vec<TextMatch>>,
}

#[derive(SimpleObject, Clone)]
pub struct TextMatch {
    pub fragment: String,
    pub property: String,
}

pub struct SearchResultItemConnection {
    page: Page<SearchResultItem>,
    kind: SearchType,
}

#[Object]
impl SearchResultItemConnection {
    async fn nodes(&self) -> Vec<Option<SearchResultItem>> {
        self.page.items.iter().cloned().map(Some).collect()
    }
    async fn edges(&self) -> Vec<SearchResultItemEdge> {
        self.page
            .items
            .iter()
            .enumerate()
            .map(|(i, n)| SearchResultItemEdge {
                cursor: encode_cursor(self.page.offset + i as i64 + 1),
                node: Some(n.clone()),
                text_matches: Some(vec![]),
            })
            .collect()
    }
    async fn page_info(&self) -> PageInfo {
        self.page.page_info()
    }
    async fn issue_count(&self) -> i32 {
        self.count(SearchType::Issue)
    }
    async fn repository_count(&self) -> i32 {
        self.count(SearchType::Repository)
    }
    async fn user_count(&self) -> i32 {
        self.count(SearchType::User)
    }
    async fn discussion_count(&self) -> i32 {
        0
    }
    async fn code_count(&self) -> i32 {
        0
    }
    async fn wiki_count(&self) -> i32 {
        0
    }
}

impl SearchResultItemConnection {
    fn count(&self, kind: SearchType) -> i32 {
        if self.kind == kind {
            self.page.total as i32
        } else {
            0
        }
    }
}

/// A parsed search query.
#[derive(Debug, Default)]
pub struct Parsed {
    pub text: Vec<String>,
    /// (qualifier, value, negated)
    pub quals: Vec<(String, String, bool)>,
}

impl Parsed {
    fn values(&self, key: &str) -> impl Iterator<Item = (&str, bool)> {
        self.quals
            .iter()
            .filter(move |q| q.0 == key)
            .map(|q| (q.1.as_str(), q.2))
    }

    fn has(&self, key: &str, value: &str) -> Option<bool> {
        self.quals
            .iter()
            .find(|q| q.0 == key && q.1.eq_ignore_ascii_case(value))
            .map(|q| !q.2)
    }
}

/// Split a query into free text and `key:value` qualifiers (quotes group).
pub fn parse(q: &str, viewer: Option<&str>) -> Parsed {
    let mut tokens: Vec<String> = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    for ch in q.chars() {
        match ch {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let mut p = Parsed::default();
    for t in tokens {
        let (neg, body) = match t.strip_prefix('-') {
            Some(rest) if rest.contains(':') => (true, rest.to_string()),
            _ => (false, t.clone()),
        };
        match body.split_once(':') {
            Some((k, v)) if !k.is_empty() && !v.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic() || c == '-') => {
                let v = if v == "@me" {
                    viewer.unwrap_or("").to_string()
                } else {
                    v.to_string()
                };
                for part in v.split(',') {
                    p.quals.push((k.to_ascii_lowercase(), part.to_string(), neg));
                }
            }
            _ => p.text.push(t),
        }
    }
    p
}

pub async fn search(
    ctx: &Context<'_>,
    query: &str,
    kind: SearchType,
    args: ConnArgs,
) -> GResult<SearchResultItemConnection> {
    let g = gql(ctx);
    let viewer = g.auth.as_ref().map(|a| a.user.login.clone());
    let parsed = parse(query, viewer.as_deref());
    let items = match kind {
        SearchType::Issue => search_issues(ctx, &parsed).await?,
        SearchType::Repository => search_repos(ctx, &parsed).await?,
        SearchType::User => search_users(ctx, &parsed).await?,
        SearchType::Discussion => vec![],
    };
    Ok(SearchResultItemConnection {
        page: Page::from_vec(items, &args)?,
        kind,
    })
}

fn push_user_match(qb: &mut QueryBuilder<'_, Postgres>, col: &str, login: &str) {
    qb.push(format!(" {col} = (SELECT id FROM users WHERE lower(login) = lower("))
        .push_bind(login.to_string())
        .push("))");
}

fn neg(qb: &mut QueryBuilder<'_, Postgres>, negated: bool) {
    qb.push(if negated { " AND NOT" } else { " AND" });
}

async fn search_issues(ctx: &Context<'_>, p: &Parsed) -> GResult<Vec<SearchResultItem>> {
    let g = gql(ctx);
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(format!(
        "SELECT {} FROM issues i JOIN repositories r ON r.id = i.repo_id
           JOIN users o ON o.id = r.owner_id
           LEFT JOIN pull_requests p ON p.issue_id = i.id WHERE true",
        db::prefixed("i", db::Issue::COLUMNS)
    ));
    for (k, v, n) in &p.quals {
        let (v, n) = (v.as_str(), *n);
        match k.as_str() {
            "repo" => {
                let (o, name) = v.split_once('/').unwrap_or(("", v));
                neg(&mut qb, n);
                qb.push(" (lower(o.login) = lower(")
                    .push_bind(o.to_string())
                    .push(") AND lower(r.name) = lower(")
                    .push_bind(name.to_string())
                    .push("))");
            }
            "user" | "org" | "owner" => {
                neg(&mut qb, n);
                qb.push(" lower(o.login) = lower(").push_bind(v.to_string()).push(")");
            }
            "is" | "state" | "type" => match v.to_ascii_lowercase().as_str() {
                "issue" => {
                    neg(&mut qb, n);
                    qb.push(" NOT i.is_pull_request");
                }
                "pr" | "pull-request" => {
                    neg(&mut qb, n);
                    qb.push(" i.is_pull_request");
                }
                "open" => {
                    neg(&mut qb, n);
                    qb.push(" i.state = 'open'");
                }
                "closed" => {
                    neg(&mut qb, n);
                    qb.push(" i.state = 'closed'");
                }
                "merged" => {
                    neg(&mut qb, n);
                    qb.push(" coalesce(p.merged, false)");
                }
                "unmerged" => {
                    neg(&mut qb, n);
                    qb.push(" (i.is_pull_request AND i.state = 'closed' AND NOT p.merged)");
                }
                "draft" => {
                    neg(&mut qb, n);
                    qb.push(" coalesce(p.draft, false)");
                }
                "public" => {
                    neg(&mut qb, n);
                    qb.push(" r.visibility = 'public'");
                }
                "private" => {
                    neg(&mut qb, n);
                    qb.push(" r.visibility <> 'public'");
                }
                "archived" => {
                    neg(&mut qb, n);
                    qb.push(" r.archived");
                }
                "locked" => {
                    neg(&mut qb, n);
                    qb.push(" i.locked");
                }
                _ => {}
            },
            "archived" => {
                neg(&mut qb, false);
                qb.push(" r.archived = ").push_bind(v == "true");
            }
            "author" => {
                neg(&mut qb, n);
                push_user_match(&mut qb, "i.author_id", v);
            }
            "assignee" => {
                neg(&mut qb, n);
                qb.push(" EXISTS (SELECT 1 FROM issue_assignees ia JOIN users u ON u.id = ia.user_id
                          WHERE ia.issue_id = i.id AND lower(u.login) = lower(")
                    .push_bind(v.to_string())
                    .push("))");
            }
            "mentions" => {
                neg(&mut qb, n);
                qb.push(" (i.body ILIKE ")
                    .push_bind(format!("%@{v}%"))
                    .push(" OR EXISTS (SELECT 1 FROM comments c WHERE c.issue_id = i.id AND c.body ILIKE ")
                    .push_bind(format!("%@{v}%"))
                    .push("))");
            }
            "commenter" => {
                neg(&mut qb, n);
                qb.push(" EXISTS (SELECT 1 FROM comments c JOIN users u ON u.id = c.author_id
                          WHERE c.issue_id = i.id AND lower(u.login) = lower(")
                    .push_bind(v.to_string())
                    .push("))");
            }
            "involves" => {
                neg(&mut qb, n);
                qb.push(" (i.author_id = (SELECT id FROM users WHERE lower(login) = lower(")
                    .push_bind(v.to_string())
                    .push(")) OR EXISTS (SELECT 1 FROM issue_assignees ia JOIN users u ON u.id = ia.user_id
                          WHERE ia.issue_id = i.id AND lower(u.login) = lower(")
                    .push_bind(v.to_string())
                    .push(")) OR i.body ILIKE ")
                    .push_bind(format!("%@{v}%"))
                    .push(" OR EXISTS (SELECT 1 FROM comments c JOIN users u ON u.id = c.author_id
                          WHERE c.issue_id = i.id AND lower(u.login) = lower(")
                    .push_bind(v.to_string())
                    .push(")))");
            }
            "review-requested" => {
                neg(&mut qb, n);
                qb.push(" EXISTS (SELECT 1 FROM pr_requested_reviewers rr
                           LEFT JOIN users u ON u.id = rr.user_id
                          WHERE rr.pull_id = i.id AND (lower(u.login) = lower(")
                    .push_bind(v.to_string())
                    .push(") OR rr.team_id IN (SELECT tm.team_id FROM team_members tm JOIN users tu ON tu.id = tm.user_id
                                                  WHERE lower(tu.login) = lower(")
                    .push_bind(v.to_string())
                    .push("))))");
            }
            "reviewed-by" => {
                neg(&mut qb, n);
                qb.push(" EXISTS (SELECT 1 FROM pr_reviews rv JOIN users u ON u.id = rv.user_id
                          WHERE rv.pull_id = i.id AND rv.state <> 'PENDING' AND lower(u.login) = lower(")
                    .push_bind(v.to_string())
                    .push("))");
            }
            "review" => match v {
                "approved" | "changes_requested" => {
                    neg(&mut qb, n);
                    qb.push(" EXISTS (SELECT 1 FROM pr_reviews rv WHERE rv.pull_id = i.id AND rv.state = ")
                        .push_bind(v.to_ascii_uppercase())
                        .push(")");
                }
                "none" => {
                    neg(&mut qb, n);
                    qb.push(" NOT EXISTS (SELECT 1 FROM pr_reviews rv WHERE rv.pull_id = i.id AND rv.state <> 'PENDING')");
                }
                "required" => {}
                _ => {}
            },
            "label" => {
                neg(&mut qb, n);
                qb.push(" EXISTS (SELECT 1 FROM issue_labels il JOIN labels l ON l.id = il.label_id
                          WHERE il.issue_id = i.id AND lower(l.name) = lower(")
                    .push_bind(v.to_string())
                    .push("))");
            }
            "milestone" => {
                neg(&mut qb, n);
                qb.push(" EXISTS (SELECT 1 FROM milestones m WHERE m.id = i.milestone_id AND lower(m.title) = lower(")
                    .push_bind(v.to_string())
                    .push("))");
            }
            "no" => match v {
                "label" => {
                    neg(&mut qb, n);
                    qb.push(" NOT EXISTS (SELECT 1 FROM issue_labels il WHERE il.issue_id = i.id)");
                }
                "assignee" => {
                    neg(&mut qb, n);
                    qb.push(" NOT EXISTS (SELECT 1 FROM issue_assignees ia WHERE ia.issue_id = i.id)");
                }
                "milestone" => {
                    neg(&mut qb, n);
                    qb.push(" i.milestone_id IS NULL");
                }
                _ => {}
            },
            "head" => {
                neg(&mut qb, n);
                qb.push(" p.head_ref = ").push_bind(v.to_string());
            }
            "base" => {
                neg(&mut qb, n);
                qb.push(" p.base_ref = ").push_bind(v.to_string());
            }
            _ => {}
        }
    }
    let in_title = p.has("in", "title") == Some(true);
    for t in &p.text {
        qb.push(" AND (i.title ILIKE ").push_bind(format!("%{t}%"));
        if !in_title {
            qb.push(" OR i.body ILIKE ").push_bind(format!("%{t}%"));
        }
        qb.push(")");
    }
    let sort = p.values("sort").next().map(|s| s.0.to_string());
    qb.push(match sort.as_deref() {
        Some("created-asc") => " ORDER BY i.created_at ASC, i.id ASC",
        Some("updated-asc") => " ORDER BY i.updated_at ASC, i.id ASC",
        Some("updated" | "updated-desc") => " ORDER BY i.updated_at DESC, i.id DESC",
        Some("comments" | "comments-desc") => " ORDER BY i.comments_count DESC, i.id DESC",
        Some("comments-asc") => " ORDER BY i.comments_count ASC, i.id ASC",
        _ => " ORDER BY i.created_at DESC, i.id DESC",
    });
    qb.push(" LIMIT ").push_bind(MAX_RESULTS * 2);
    let rows: Vec<db::Issue> = qb.build_query_as().fetch_all(&g.state.db).await.gql()?;
    let readable = readable_repos(ctx, rows.iter().map(|i| i.repo_id)).await?;
    let rows: Vec<db::Issue> = rows
        .into_iter()
        .filter(|i| readable.contains_key(&i.repo_id))
        .take(MAX_RESULTS as usize)
        .collect();
    // Keep order while attaching PR rows.
    let pr_rows: Vec<db::Issue> = rows.iter().filter(|i| i.is_pull_request).cloned().collect();
    let prs: HashMap<i64, PullRequest> = from_issues(ctx, pr_rows)
        .await?
        .into_iter()
        .map(|p| (p.issue().id, p))
        .collect();
    Ok(rows
        .into_iter()
        .filter_map(|i| {
            if i.is_pull_request {
                prs.get(&i.id).cloned().map(SearchResultItem::PullRequest)
            } else {
                Some(SearchResultItem::Issue(Issue::new(Arc::new(i))))
            }
        })
        .collect())
}

/// Readable repositories among `ids` (one batched permission check).
async fn readable_repos(
    ctx: &Context<'_>,
    ids: impl IntoIterator<Item = i64>,
) -> GResult<HashMap<i64, Repository>> {
    let g = gql(ctx);
    let mut ids: Vec<i64> = ids.into_iter().collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let repos: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1)",
        db::Repository::COLUMNS
    ))
    .bind(&ids)
    .fetch_all(&g.state.db)
    .await
    .gql()?;
    let rows = RepoLoader::rows(&g.state, g.auth.as_ref(), repos)
        .await
        .gql()?;
    Ok(rows
        .into_iter()
        .filter(|r| r.perm >= Permission::Read)
        .map(|r| (r.repo.id, Repository(Arc::new(r))))
        .collect())
}

async fn search_repos(ctx: &Context<'_>, p: &Parsed) -> GResult<Vec<SearchResultItem>> {
    let g = gql(ctx);
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT r.id FROM repositories r JOIN users o ON o.id = r.owner_id WHERE true",
    );
    for (k, v, n) in &p.quals {
        let (v, n) = (v.as_str(), *n);
        match k.as_str() {
            "repo" => {
                let (o, name) = v.split_once('/').unwrap_or(("", v));
                neg(&mut qb, n);
                qb.push(" (lower(o.login) = lower(")
                    .push_bind(o.to_string())
                    .push(") AND lower(r.name) = lower(")
                    .push_bind(name.to_string())
                    .push("))");
            }
            "user" | "org" | "owner" => {
                neg(&mut qb, n);
                qb.push(" lower(o.login) = lower(").push_bind(v.to_string()).push(")");
            }
            "is" => match v {
                "public" => {
                    neg(&mut qb, n);
                    qb.push(" r.visibility = 'public'");
                }
                "private" | "internal" => {
                    neg(&mut qb, n);
                    qb.push(" r.visibility <> 'public'");
                }
                "archived" => {
                    neg(&mut qb, n);
                    qb.push(" r.archived");
                }
                "fork" => {
                    neg(&mut qb, n);
                    qb.push(" r.fork");
                }
                "template" => {
                    neg(&mut qb, n);
                    qb.push(" r.is_template");
                }
                _ => {}
            },
            "archived" => {
                qb.push(" AND r.archived = ").push_bind(v == "true");
            }
            "fork" => match v {
                "true" | "only" => {
                    qb.push(" AND r.fork");
                }
                "false" => {
                    qb.push(" AND NOT r.fork");
                }
                _ => {}
            },
            "language" => {
                neg(&mut qb, n);
                qb.push(" lower(coalesce(r.language, '')) = lower(")
                    .push_bind(v.to_string())
                    .push(")");
            }
            "topic" => {
                neg(&mut qb, n);
                qb.push(" ").push_bind(v.to_lowercase()).push(" = ANY(r.topics)");
            }
            _ => {}
        }
    }
    let in_name = p.has("in", "name") == Some(true);
    for t in &p.text {
        qb.push(" AND (r.name ILIKE ").push_bind(format!("%{t}%"));
        if !in_name {
            qb.push(" OR coalesce(r.description, '') ILIKE ")
                .push_bind(format!("%{t}%"));
        }
        qb.push(")");
    }
    let sort = p.values("sort").next().map(|s| s.0.to_string());
    qb.push(match sort.as_deref() {
        Some("stars" | "stars-desc") => " ORDER BY r.stargazers_count DESC, r.id DESC",
        Some("forks" | "forks-desc") => " ORDER BY r.forks_count DESC, r.id DESC",
        Some("updated" | "updated-desc") => " ORDER BY r.updated_at DESC, r.id DESC",
        _ => " ORDER BY coalesce(r.pushed_at, r.created_at) DESC, r.id DESC",
    });
    qb.push(" LIMIT ").push_bind(MAX_RESULTS * 2);
    let ids: Vec<i64> = qb.build_query_scalar().fetch_all(&g.state.db).await.gql()?;
    let readable = readable_repos(ctx, ids.iter().copied()).await?;
    Ok(ids
        .iter()
        .filter_map(|id| readable.get(id).cloned())
        .take(MAX_RESULTS as usize)
        .map(SearchResultItem::Repository)
        .collect())
}

async fn search_users(ctx: &Context<'_>, p: &Parsed) -> GResult<Vec<SearchResultItem>> {
    let g = gql(ctx);
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(format!(
        "SELECT {} FROM users WHERE suspended_at IS NULL AND type <> 'Bot'",
        db::User::COLUMNS
    ));
    for (k, v, _) in &p.quals {
        if k == "type" {
            match v.as_str() {
                "user" => {
                    qb.push(" AND type = 'User'");
                }
                "org" => {
                    qb.push(" AND type = 'Organization'");
                }
                _ => {}
            }
        }
    }
    for t in &p.text {
        qb.push(" AND (login ILIKE ")
            .push_bind(format!("%{t}%"))
            .push(" OR coalesce(name, '') ILIKE ")
            .push_bind(format!("%{t}%"))
            .push(")");
    }
    qb.push(" ORDER BY lower(login) LIMIT ").push_bind(MAX_RESULTS);
    let rows: Vec<db::User> = qb.build_query_as().fetch_all(&g.state.db).await.gql()?;
    Ok(rows
        .into_iter()
        .map(|u| {
            if u.is_org() {
                SearchResultItem::Organization(Organization(Arc::new(u)))
            } else {
                SearchResultItem::User(User(Arc::new(u)))
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qualifiers() {
        let p = parse(r#"is:open -label:bug "two words" assignee:@me repo:a/b"#, Some("me"));
        assert_eq!(p.text, vec!["two words"]);
        assert!(p.quals.contains(&("label".into(), "bug".into(), true)));
        assert!(p.quals.contains(&("assignee".into(), "me".into(), false)));
        assert!(p.quals.contains(&("repo".into(), "a/b".into(), false)));
    }
}
