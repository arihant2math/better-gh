//! `GET /search/repositories` and `GET /search/topics`.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::HeaderMap;
use bgh_core::models::api::MinimalRepository;
use bgh_core::perms::{self, ReadableRepos};
use bgh_core::prelude::*;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, Postgres, QueryBuilder};

use crate::common::{
    Scored, SearchParams, SearchResult, check_window, page_limit, text_match, wants_text_matches,
};
use crate::issues::websearch_text;
use crate::query::{DateRange, NumRange, Query as SearchQuery, Term, comma_list, like_escape};
use crate::resolve::{self, Users};
use crate::sqlb::{Arg, Conds, Sql, date_range, num_range, readable};

fn invalid(msg: &str) -> ApiError {
    ApiError::invalid_field(FieldError {
        message: Some(msg.to_string()),
        ..FieldError::new("Search", "q", "invalid")
    })
}

const DOC: &str = "(setweight(to_tsvector('simple', r.name), 'A') || \
    setweight(to_tsvector('english', coalesce(r.description, '')), 'B'))";

/// WHERE conditions over `repositories r` (and `users o` = owner).
pub async fn filters(
    state: &AppState,
    auth: Option<&AuthContext>,
    readable_set: &ReadableRepos,
    q: &SearchQuery,
) -> ApiResult<(Conds, Option<String>)> {
    let mut logins = Vec::new();
    let mut repo_names = Vec::new();
    for t in &q.terms {
        if let Some((k, v, _)) = t.qualifier() {
            match k {
                "user" | "org" => logins.extend(comma_list(v)),
                "repo" => repo_names.extend(comma_list(v)),
                _ => {}
            }
        }
    }
    let users = Users::load(state, auth, logins).await?;
    let repos = resolve::repos(state, readable_set, repo_names).await?;

    let mut c = Conds::default();
    c.push(readable(readable_set, "r"));
    let text = websearch_text(q);
    let has_text = q.terms.iter().any(|t| matches!(t, Term::Text { .. }));
    let mut fields: Vec<String> = q.quals("in").flat_map(|(v, _)| comma_list(v)).collect();
    fields.iter_mut().for_each(|f| *f = f.to_lowercase());
    if has_text {
        let positive = q.texts();
        c.with(|s| {
            let mut first = true;
            let mut or = |s: &mut Sql| {
                if !first {
                    s.raw(" OR ");
                }
                first = false;
            };
            let all = fields.is_empty();
            if all || fields.iter().any(|f| f == "name") {
                // Every positive term is a substring of the name.
                if !positive.is_empty() {
                    or(s);
                    s.raw("(");
                    for (i, t) in positive.iter().enumerate() {
                        if i > 0 {
                            s.raw(" AND ");
                        }
                        s.raw("lower(r.name) LIKE ")
                            .text(format!("%{}%", like_escape(&t.to_lowercase())));
                    }
                    s.raw(")");
                }
            }
            if all {
                or(s);
                s.raw(format!("{DOC} @@ websearch_to_tsquery('english', "))
                    .text(text.clone())
                    .raw(format!(") OR {DOC} @@ websearch_to_tsquery('simple', "))
                    .text(text.clone())
                    .raw(")");
            } else if fields.iter().any(|f| f == "description" || f == "readme") {
                or(s);
                s.raw("to_tsvector('english', coalesce(r.description, '')) @@ websearch_to_tsquery('english', ")
                    .text(text.clone())
                    .raw(")");
            }
            if all || fields.iter().any(|f| f == "topics") {
                or(s);
                s.raw("r.topics && ").arg(Arg::Texts(
                    positive.iter().map(|t| t.to_lowercase()).collect(),
                ));
            }
        });
    }

    let mut scope = Sql::new();
    let mut n = 0;
    let mut forks = "exclude";
    for t in &q.terms {
        let Some((key, value, neg)) = t.qualifier() else {
            continue;
        };
        let not = if neg { "NOT " } else { "" };
        let mut s = Sql::new();
        match key {
            "user" | "org" | "repo" if !neg => {
                if n > 0 {
                    scope.raw(" OR ");
                }
                n += 1;
                if key == "repo" {
                    forks = "include";
                    let ids: Vec<i64> = comma_list(value)
                        .iter()
                        .map(|r| repos.get(&r.to_lowercase()).copied().unwrap_or(0))
                        .collect();
                    scope.raw("r.id = ANY(").arg(Arg::I64s(ids)).raw(")");
                } else {
                    scope
                        .raw("r.owner_id = ANY(")
                        .arg(Arg::I64s(users.ids(&comma_list(value))))
                        .raw(")");
                }
            }
            "user" | "org" => {
                s.raw("NOT r.owner_id = ANY(")
                    .arg(Arg::I64s(users.ids(&comma_list(value))))
                    .raw(")");
            }
            "repo" => {
                let ids: Vec<i64> = comma_list(value)
                    .iter()
                    .map(|r| repos.get(&r.to_lowercase()).copied().unwrap_or(0))
                    .collect();
                s.raw("NOT r.id = ANY(").arg(Arg::I64s(ids)).raw(")");
            }
            "language" => {
                s.raw(format!("{not}coalesce(lower(r.language) = ANY("))
                    .arg(Arg::Texts(
                        comma_list(value).iter().map(|l| l.to_lowercase()).collect(),
                    ))
                    .raw("), false)");
            }
            "topic" => {
                s.raw(format!("{not}r.topics @> ARRAY["))
                    .text(value.to_lowercase())
                    .raw("]::text[]");
            }
            "topics" => {
                let r = NumRange::parse(value).ok_or_else(|| invalid("Invalid topics: range"))?;
                s.raw(format!("{not}("));
                num_range(&mut s, "cardinality(r.topics)", &r);
                s.raw(")");
            }
            "stars" | "forks" | "size" | "help-wanted-issues" | "good-first-issues" => {
                let r = NumRange::parse(value)
                    .ok_or_else(|| invalid(&format!("Invalid {key}: range")))?;
                let expr = match key {
                    "stars" => "r.stargazers_count",
                    "forks" => "r.forks_count",
                    "size" => "r.size",
                    _ => "r.open_issues_count",
                };
                s.raw(format!("{not}("));
                num_range(&mut s, expr, &r);
                s.raw(")");
            }
            "created" | "pushed" | "updated" => {
                let r = DateRange::parse(value)
                    .ok_or_else(|| invalid(&format!("Invalid date for {key}: qualifier")))?;
                let expr = match key {
                    "created" => "r.created_at",
                    "pushed" => "r.pushed_at",
                    _ => "r.updated_at",
                };
                s.raw(format!("{not}coalesce(("));
                date_range(&mut s, expr, &r);
                s.raw("), false)");
            }
            "is" => match value.to_lowercase().as_str() {
                "public" => {
                    s.raw(format!("{not}r.visibility = 'public'"));
                }
                "private" | "internal" => {
                    s.raw(format!("{not}r.visibility <> 'public'"));
                }
                "template" => {
                    s.raw(format!("{not}r.is_template"));
                }
                "archived" => {
                    s.raw(format!("{not}r.archived"));
                }
                "fork" => {
                    forks = "include";
                    s.raw(format!("{not}r.fork"));
                }
                _ => return Err(invalid("Invalid value for is: qualifier")),
            },
            "archived" => {
                s.raw("r.archived = ")
                    .arg(Arg::Bool(value.eq_ignore_ascii_case("true") != neg));
            }
            "template" => {
                s.raw("r.is_template = ")
                    .arg(Arg::Bool(value.eq_ignore_ascii_case("true") != neg));
            }
            "fork" => match value.to_lowercase().as_str() {
                "true" => forks = "include",
                "only" => forks = "only",
                "false" => forks = "exclude",
                _ => return Err(invalid("fork must be true, false or only")),
            },
            "license" => {
                s.raw(format!("{not}coalesce(lower(r.license_spdx_id) = "))
                    .text(value.to_lowercase())
                    .raw(", false)");
            }
            "mirror" if value.eq_ignore_ascii_case("true") != neg => {
                s.raw("FALSE");
            }
            _ => {}
        }
        c.push(s);
    }
    c.push(scope);
    match forks {
        "only" => c.with(|s| {
            s.raw("r.fork");
        }),
        "exclude" => c.with(|s| {
            s.raw("NOT r.fork");
        }),
        _ => {}
    }
    Ok((c, has_text.then_some(text)))
}

#[derive(FromRow)]
struct Hit {
    #[sqlx(flatten)]
    repo: db::Repository,
    score: f32,
}

/// `GET /search/repositories`
pub async fn search(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Query(params): Query<SearchParams>,
) -> ApiResult<SearchResult<Scored<MinimalRepository>>> {
    let (_, q) = params.query()?;
    check_window(&p)?;
    let readable_set = perms::readable_repos(&state.db, auth.as_ref()).await?;
    let (conds, text) = filters(&state, auth.as_ref(), &readable_set, &q).await?;
    let where_sql = conds.to_sql();
    let dir = if params.ascending() { "ASC" } else { "DESC" };
    let order = match params.sort() {
        None | Some("best-match") => {
            if text.is_some() {
                format!("score {dir}, r.stargazers_count DESC, r.id")
            } else {
                format!("r.stargazers_count {dir}, r.id")
            }
        }
        Some("stars") => format!("r.stargazers_count {dir}, r.id {dir}"),
        Some("forks") => format!("r.forks_count {dir}, r.id {dir}"),
        Some("help-wanted-issues") => format!("r.open_issues_count {dir}, r.id {dir}"),
        Some("updated") => format!("r.updated_at {dir}, r.id {dir}"),
        Some(_) => return Err(invalid("Invalid sort")),
    };

    let mut count_q: QueryBuilder<Postgres> =
        QueryBuilder::new("SELECT count(*) FROM repositories r WHERE ");
    where_sql.build(&mut count_q);
    let total: i64 = count_q.build_query_scalar().fetch_one(&state.db).await?;

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("SELECT ");
    qb.push(db::prefixed("r", db::Repository::COLUMNS));
    match &text {
        Some(t) => {
            let lower = t.to_lowercase();
            qb.push(format!(
                ", (ts_rank({DOC}, websearch_to_tsquery('english', "
            ))
            .push_bind(t.clone())
            .push(")) + CASE WHEN lower(r.name) = ")
            .push_bind(lower)
            .push(" THEN 1 ELSE 0 END)::real AS score");
        }
        None => {
            qb.push(", 1::real AS score");
        }
    }
    qb.push(" FROM repositories r WHERE ");
    where_sql.build(&mut qb);
    qb.push(format!(" ORDER BY {order} LIMIT "))
        .push_bind(page_limit(&p))
        .push(" OFFSET ")
        .push_bind(p.offset());
    let hits: Vec<Hit> = qb.build_query_as().fetch_all(&state.db).await?;
    let scores: HashMap<i64, f32> = hits.iter().map(|h| (h.repo.id, h.score)).collect();
    let rendered = bgh_core::views::minimal_repos(
        &state,
        auth.as_ref(),
        hits.into_iter().map(|h| h.repo).collect(),
    )
    .await?;
    let matches = wants_text_matches(&headers);
    let needles = q.texts();
    let items = rendered
        .into_iter()
        .map(|r| {
            let text_matches = matches.then(|| {
                let mut v = Vec::new();
                v.extend(text_match(
                    &r.url,
                    "Repository",
                    "name",
                    &r.name,
                    &needles,
                    200,
                ));
                if let Some(d) = &r.description {
                    v.extend(text_match(
                        &r.url,
                        "Repository",
                        "description",
                        d,
                        &needles,
                        300,
                    ));
                }
                v
            });
            Scored {
                score: if text.is_some() {
                    f64::from(scores.get(&r.id).copied().unwrap_or(1.0))
                } else {
                    1.0
                },
                item: r,
                text_matches,
            }
        })
        .collect();
    Ok(SearchResult::new(&p, total, items, false))
}

/// `topic-search-result-item`.
#[derive(Debug, Clone, Serialize)]
pub struct Topic {
    pub name: String,
    pub display_name: Option<String>,
    pub short_description: Option<String>,
    pub description: Option<String>,
    pub created_by: Option<String>,
    pub released: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub featured: bool,
    pub curated: bool,
    pub repository_count: i64,
    pub logo_url: Option<String>,
    pub related: Option<Vec<serde_json::Value>>,
    pub aliases: Option<Vec<serde_json::Value>>,
}

#[derive(FromRow)]
struct TopicRow {
    name: String,
    n: i64,
    first: DateTime<Utc>,
    last: DateTime<Utc>,
}

/// `GET /search/topics`: topics used by repositories the caller can read.
pub async fn topics(
    State(state): State<AppState>,
    auth: MaybeUser,
    p: Pagination,
    Query(params): Query<SearchParams>,
) -> ApiResult<SearchResult<Scored<Topic>>> {
    let (_, q) = params.query()?;
    check_window(&p)?;
    let readable_set = perms::readable_repos(&state.db, auth.as_ref()).await?;
    let mut having = Sql::new();
    having.raw("TRUE");
    for (v, neg) in q.quals("repositories") {
        let r = NumRange::parse(v).ok_or_else(|| invalid("Invalid repositories: range"))?;
        having.raw(if neg { " AND NOT (" } else { " AND (" });
        num_range(&mut having, "count(*)", &r);
        having.raw(")");
    }
    let mut cond = Sql::new();
    cond.raw("(").append(&readable(&readable_set, "r")).raw(")");
    for t in q.texts() {
        cond.raw(" AND t LIKE ")
            .text(format!("%{}%", like_escape(&t.to_lowercase())));
    }
    for (v, _) in q.quals("is") {
        if matches!(v, "featured" | "curated") {
            cond.raw(" AND FALSE");
        }
    }
    let exact = q.text().to_lowercase();
    let build = |qb: &mut QueryBuilder<Postgres>| {
        qb.push(" FROM repositories r, unnest(r.topics) t WHERE ");
        cond.build(qb);
        qb.push(" GROUP BY t HAVING ");
        having.build(qb);
    };
    let mut count_q: QueryBuilder<Postgres> = QueryBuilder::new("SELECT count(*) FROM (SELECT t");
    build(&mut count_q);
    count_q.push(") x");
    let total: i64 = count_q.build_query_scalar().fetch_one(&state.db).await?;
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT t AS name, count(*) AS n, min(r.created_at) AS first, max(r.updated_at) AS last",
    );
    build(&mut qb);
    qb.push(" ORDER BY (t = ")
        .push_bind(exact.clone())
        .push(") DESC, count(*) DESC, t LIMIT ")
        .push_bind(page_limit(&p))
        .push(" OFFSET ")
        .push_bind(p.offset());
    let rows: Vec<TopicRow> = qb.build_query_as().fetch_all(&state.db).await?;
    let items = rows
        .into_iter()
        .map(|r| Scored {
            score: if r.name == exact { 2.0 } else { 1.0 },
            item: Topic {
                display_name: None,
                short_description: None,
                description: None,
                created_by: None,
                released: None,
                created_at: r.first.into(),
                updated_at: r.last.into(),
                featured: false,
                curated: false,
                repository_count: r.n,
                logo_url: None,
                related: None,
                aliases: None,
                name: r.name,
            },
            text_matches: None,
        })
        .collect();
    Ok(SearchResult::new(&p, total, items, false))
}
