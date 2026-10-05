//! `GET /search/commits` over the commit index (default branches).

use std::collections::HashMap;

use axum::extract::State;
use axum::http::HeaderMap;
use bgh_core::models::api::{MinimalRepository, SimpleUser};
use bgh_core::node_id::{self, NodeType};
use bgh_core::perms;
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, Postgres, QueryBuilder};

use crate::common::{
    Scored, SearchParams, SearchResult, check_window, page_limit, text_match, wants_text_matches,
};
use crate::issues::websearch_text;
use crate::query::{DateRange, Term, comma_list};
use crate::resolve::{self, Users};
use crate::sqlb::{Arg, Conds, Sql, date_range, readable};

fn invalid(msg: &str) -> ApiError {
    ApiError::invalid_field(FieldError {
        message: Some(msg.to_string()),
        ..FieldError::new("Search", "q", "invalid")
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct GitActor {
    pub name: String,
    pub email: String,
    pub date: Timestamp,
}

#[derive(Debug, Clone, Serialize)]
pub struct TreeRef {
    pub url: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitInner {
    pub url: String,
    pub author: GitActor,
    pub committer: GitActor,
    pub message: String,
    pub tree: TreeRef,
    pub comment_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParentRef {
    pub url: String,
    pub html_url: String,
    pub sha: String,
}

/// `commit-search-result-item`.
#[derive(Debug, Clone, Serialize)]
pub struct CommitItem {
    pub url: String,
    pub sha: String,
    pub node_id: String,
    pub html_url: String,
    pub comments_url: String,
    pub commit: CommitInner,
    pub author: Option<SimpleUser>,
    pub committer: Option<SimpleUser>,
    pub parents: Vec<ParentRef>,
    pub repository: MinimalRepository,
}

#[derive(FromRow)]
struct Hit {
    repo_id: i64,
    sha: String,
    tree_sha: String,
    parents: Vec<String>,
    message: String,
    author_name: String,
    author_email: String,
    author_date: DateTime<Utc>,
    committer_name: String,
    committer_email: String,
    committer_date: DateTime<Utc>,
    author_id: Option<i64>,
    committer_id: Option<i64>,
    score: f32,
}

/// `GET /search/commits`
pub async fn search(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Query(params): Query<SearchParams>,
) -> ApiResult<SearchResult<Scored<CommitItem>>> {
    let (_, q) = params.query()?;
    check_window(&p)?;
    let readable_set = perms::readable_repos(&state.db, auth.as_ref()).await?;
    let mut logins = Vec::new();
    let mut repo_names = Vec::new();
    for t in &q.terms {
        if let Some((k, v, _)) = t.qualifier() {
            match k {
                "user" | "org" | "author" | "committer" => logins.extend(comma_list(v)),
                "repo" => repo_names.extend(comma_list(v)),
                _ => {}
            }
        }
    }
    let users = Users::load(&state, auth.as_ref(), logins).await?;
    let repos = resolve::repos(&state, &readable_set, repo_names).await?;

    let mut c = Conds::default();
    c.push(readable(&readable_set, "r"));
    let text = websearch_text(&q);
    let has_text = q.terms.iter().any(|t| matches!(t, Term::Text { .. }));
    if has_text {
        c.with(|s| {
            s.raw("c.search @@ websearch_to_tsquery('english', ")
                .text(text.clone())
                .raw(")");
        });
    }
    let mut scope = Sql::new();
    let mut n = 0;
    for t in &q.terms {
        let Some((key, value, neg)) = t.qualifier() else {
            continue;
        };
        let not = if neg { "NOT " } else { "" };
        let mut s = Sql::new();
        match key {
            "repo" | "user" | "org" if !neg => {
                if n > 0 {
                    scope.raw(" OR ");
                }
                n += 1;
                if key == "repo" {
                    let ids: Vec<i64> = comma_list(value)
                        .iter()
                        .map(|r| repos.get(&r.to_lowercase()).copied().unwrap_or(0))
                        .collect();
                    scope.raw("c.repo_id = ANY(").arg(Arg::I64s(ids)).raw(")");
                } else {
                    scope
                        .raw("r.owner_id = ANY(")
                        .arg(Arg::I64s(users.ids(&comma_list(value))))
                        .raw(")");
                }
            }
            "repo" => {
                let ids: Vec<i64> = comma_list(value)
                    .iter()
                    .map(|r| repos.get(&r.to_lowercase()).copied().unwrap_or(0))
                    .collect();
                s.raw("NOT c.repo_id = ANY(").arg(Arg::I64s(ids)).raw(")");
            }
            "user" | "org" => {
                s.raw("NOT r.owner_id = ANY(")
                    .arg(Arg::I64s(users.ids(&comma_list(value))))
                    .raw(")");
            }
            "author" | "committer" => {
                s.raw(format!("{not}coalesce(c.{key}_id = ANY("))
                    .arg(Arg::I64s(users.ids(&comma_list(value))))
                    .raw("), false)");
            }
            "author-name" | "committer-name" => {
                let col = key.replace('-', "_");
                s.raw(format!("{not}lower(c.{col}) LIKE ")).text(format!(
                    "%{}%",
                    crate::query::like_escape(&value.to_lowercase())
                ));
            }
            "author-email" | "committer-email" => {
                let col = key.replace('-', "_");
                s.raw(format!("{not}lower(c.{col}) = "))
                    .text(value.to_lowercase());
            }
            "author-date" | "committer-date" => {
                let r = DateRange::parse(value)
                    .ok_or_else(|| invalid(&format!("Invalid date for {key}: qualifier")))?;
                let col = format!("c.{}", key.replace('-', "_"));
                s.raw(format!("{not}("));
                date_range(&mut s, &col, &r);
                s.raw(")");
            }
            "merge" => {
                let merge = value.eq_ignore_ascii_case("true") != neg;
                s.raw(if merge {
                    "cardinality(c.parents) > 1"
                } else {
                    "cardinality(c.parents) <= 1"
                });
            }
            "hash" => {
                s.raw(format!("{not}c.sha LIKE "))
                    .text(format!("{}%", value.to_lowercase()));
            }
            "parent" => {
                s.raw(format!(
                    "{not}EXISTS (SELECT 1 FROM unnest(c.parents) x WHERE x LIKE "
                ))
                .text(format!("{}%", value.to_lowercase()))
                .raw(")");
            }
            "tree" => {
                s.raw(format!("{not}c.tree_sha LIKE "))
                    .text(format!("{}%", value.to_lowercase()));
            }
            "is" => match value.to_lowercase().as_str() {
                "public" => {
                    s.raw(format!("{not}r.visibility = 'public'"));
                }
                "private" => {
                    s.raw(format!("{not}r.visibility <> 'public'"));
                }
                _ => return Err(invalid("Invalid value for is: qualifier")),
            },
            _ => {}
        }
        c.push(s);
    }
    c.push(scope);
    let where_sql = c.to_sql();
    let dir = if params.ascending() { "ASC" } else { "DESC" };
    let order = match params.sort() {
        None | Some("best-match") => {
            if has_text {
                format!("score {dir}, c.committer_date DESC, c.sha")
            } else {
                format!("c.committer_date {dir}, c.sha")
            }
        }
        Some("author-date") => format!("c.author_date {dir}, c.sha"),
        Some("committer-date") => format!("c.committer_date {dir}, c.sha"),
        Some(_) => return Err(invalid("Invalid sort")),
    };
    let from = " FROM commit_index c JOIN repositories r ON r.id = c.repo_id WHERE ";
    let mut count_q: QueryBuilder<Postgres> = QueryBuilder::new("SELECT count(*)");
    count_q.push(from);
    where_sql.build(&mut count_q);
    let total: i64 = count_q.build_query_scalar().fetch_one(&state.db).await?;

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT c.repo_id, c.sha, c.tree_sha, c.parents, c.message, c.author_name, c.author_email,
                c.author_date, c.committer_name, c.committer_email, c.committer_date,
                c.author_id, c.committer_id, ",
    );
    if has_text {
        qb.push("ts_rank(c.search, websearch_to_tsquery('english', ")
            .push_bind(text.clone())
            .push("))::real AS score");
    } else {
        qb.push("1::real AS score");
    }
    qb.push(from);
    where_sql.build(&mut qb);
    qb.push(format!(" ORDER BY {order} LIMIT "))
        .push_bind(page_limit(&p))
        .push(" OFFSET ")
        .push_bind(p.offset());
    let hits: Vec<Hit> = qb.build_query_as().fetch_all(&state.db).await?;

    let mut repo_ids: Vec<i64> = hits.iter().map(|h| h.repo_id).collect();
    repo_ids.sort_unstable();
    repo_ids.dedup();
    let repo_rows: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1)",
        db::Repository::COLUMNS
    ))
    .bind(&repo_ids)
    .fetch_all(&state.db)
    .await?;
    let rendered: HashMap<i64, MinimalRepository> =
        bgh_core::views::minimal_repos(&state, auth.as_ref(), repo_rows)
            .await?
            .into_iter()
            .map(|r| (r.id, r))
            .collect();
    let people = bgh_core::views::users_by_id(
        &state,
        hits.iter().flat_map(|h| [h.author_id, h.committer_id]),
    )
    .await?;
    let matches = wants_text_matches(&headers);
    let needles = q.texts();
    let urls = &state.urls;
    let items = hits
        .into_iter()
        .filter_map(|h| {
            let repository = rendered.get(&h.repo_id)?.clone();
            let (owner, name) = repository.full_name.split_once('/')?;
            let (owner, name) = (owner.to_string(), name.to_string());
            let url = urls.commit(&owner, &name, &h.sha);
            let text_matches = matches.then(|| {
                text_match(&url, "Commit", "message", &h.message, &needles, 300)
                    .into_iter()
                    .collect()
            });
            Some(Scored {
                item: CommitItem {
                    html_url: urls.commit_html(&owner, &name, &h.sha),
                    comments_url: format!("{url}/comments"),
                    node_id: node_id::encode_str(
                        NodeType::Commit,
                        &format!("{}:{}", h.repo_id, h.sha),
                    ),
                    commit: CommitInner {
                        url: urls.api(&format!("/repos/{owner}/{name}/git/commits/{}", h.sha)),
                        author: GitActor {
                            name: h.author_name,
                            email: h.author_email,
                            date: h.author_date.into(),
                        },
                        committer: GitActor {
                            name: h.committer_name,
                            email: h.committer_email,
                            date: h.committer_date.into(),
                        },
                        message: h.message.trim_end().to_string(),
                        tree: TreeRef {
                            url: urls
                                .api(&format!("/repos/{owner}/{name}/git/trees/{}", h.tree_sha)),
                            sha: h.tree_sha,
                        },
                        comment_count: 0,
                    },
                    author: h
                        .author_id
                        .and_then(|id| people.get(&id))
                        .map(|u| SimpleUser::new(urls, u)),
                    committer: h
                        .committer_id
                        .and_then(|id| people.get(&id))
                        .map(|u| SimpleUser::new(urls, u)),
                    parents: h
                        .parents
                        .iter()
                        .map(|p| ParentRef {
                            url: urls.commit(&owner, &name, p),
                            html_url: urls.commit_html(&owner, &name, p),
                            sha: p.clone(),
                        })
                        .collect(),
                    repository,
                    url,
                    sha: h.sha,
                },
                score: if has_text { f64::from(h.score) } else { 1.0 },
                text_matches,
            })
        })
        .collect();
    Ok(SearchResult::new(&p, total, items, false))
}
