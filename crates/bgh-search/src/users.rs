//! `GET /search/users` and `GET /search/labels`.

use axum::extract::State;
use axum::http::HeaderMap;
use bgh_core::models::api::{Label, SimpleUser};
use bgh_core::prelude::*;
use serde::Deserialize;
use sqlx::{FromRow, Postgres, QueryBuilder};

use crate::common::{
    Scored, SearchParams, SearchResult, check_window, page_limit, text_match, wants_text_matches,
};
use crate::query::{DateRange, NumRange, comma_list, like_escape};
use crate::sqlb::{Arg, Conds, Sql, date_range, num_range};

fn invalid(msg: &str) -> ApiError {
    ApiError::invalid_field(FieldError {
        message: Some(msg.to_string()),
        ..FieldError::new("Search", "q", "invalid")
    })
}

const FOLLOWERS: &str = "(SELECT count(*) FROM follows f WHERE f.following_id = u.id)";
const REPOS: &str =
    "(SELECT count(*) FROM repositories x WHERE x.owner_id = u.id AND x.visibility = 'public')";

#[derive(FromRow)]
struct Hit {
    #[sqlx(flatten)]
    user: db::User,
    score: f32,
}

/// `GET /search/users`
pub async fn search(
    State(state): State<AppState>,
    _auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Query(params): Query<SearchParams>,
) -> ApiResult<SearchResult<Scored<SimpleUser>>> {
    let (_, q) = params.query()?;
    check_window(&p)?;
    let mut c = Conds::default();
    c.with(|s| {
        s.raw("u.suspended_at IS NULL");
    });
    let mut fields: Vec<String> = q
        .quals("in")
        .flat_map(|(v, _)| comma_list(v))
        .map(|f| f.to_lowercase())
        .collect();
    if fields.is_empty() {
        fields = vec!["login".into(), "name".into(), "email".into()];
    }
    let texts: Vec<String> = q.texts().iter().map(|t| t.to_lowercase()).collect();
    for t in &texts {
        let pat = format!("%{}%", like_escape(t));
        c.with(|s| {
            for (i, f) in fields.iter().enumerate() {
                if i > 0 {
                    s.raw(" OR ");
                }
                let col = match f.as_str() {
                    "name" | "fullname" => "lower(coalesce(u.name, ''))",
                    "email" => "lower(coalesce(u.email, ''))",
                    _ => "lower(u.login)",
                };
                s.raw(format!("{col} LIKE ")).text(pat.clone());
            }
        });
    }
    for t in q.negated_texts() {
        let pat = format!("%{}%", like_escape(&t.to_lowercase()));
        c.with(|s| {
            s.raw("NOT lower(u.login) LIKE ").text(pat);
        });
    }
    for t in &q.terms {
        let Some((key, value, neg)) = t.qualifier() else {
            continue;
        };
        let not = if neg { "NOT " } else { "" };
        let mut s = Sql::new();
        match key {
            "type" => match value.to_lowercase().as_str() {
                "user" => {
                    s.raw(format!("{not}u.type = 'User'"));
                }
                "org" | "organization" => {
                    s.raw(format!("{not}u.type = 'Organization'"));
                }
                _ => return Err(invalid("type must be user or org")),
            },
            "user" | "org" => {
                s.raw(format!("{not}lower(u.login) = ANY("))
                    .arg(Arg::Texts(
                        comma_list(value).iter().map(|v| v.to_lowercase()).collect(),
                    ))
                    .raw(")");
            }
            "repos" | "followers" => {
                let r = NumRange::parse(value)
                    .ok_or_else(|| invalid(&format!("Invalid {key}: range")))?;
                s.raw(format!("{not}("));
                num_range(&mut s, if key == "repos" { REPOS } else { FOLLOWERS }, &r);
                s.raw(")");
            }
            "created" => {
                let r = DateRange::parse(value).ok_or_else(|| invalid("Invalid created: date"))?;
                s.raw(format!("{not}("));
                date_range(&mut s, "u.created_at", &r);
                s.raw(")");
            }
            "location" => {
                s.raw(format!("{not}coalesce(lower(u.location) LIKE "))
                    .text(format!("%{}%", like_escape(&value.to_lowercase())))
                    .raw(", false)");
            }
            "language" => {
                s.raw(format!(
                    "{not}EXISTS (SELECT 1 FROM repositories x WHERE x.owner_id = u.id AND x.visibility = 'public' AND lower(x.language) = "
                ))
                .text(value.to_lowercase())
                .raw(")");
            }
            "fullname" => {
                s.raw(format!("{not}coalesce(lower(u.name) LIKE "))
                    .text(format!("%{}%", like_escape(&value.to_lowercase())))
                    .raw(", false)");
            }
            _ => {}
        }
        c.push(s);
    }
    let where_sql = c.to_sql();
    let dir = if params.ascending() { "ASC" } else { "DESC" };
    let order = match params.sort() {
        None | Some("best-match") => format!("score {dir}, {FOLLOWERS} DESC, u.id"),
        Some("followers") => format!("{FOLLOWERS} {dir}, u.id {dir}"),
        Some("repositories") => format!("{REPOS} {dir}, u.id {dir}"),
        Some("joined") => format!("u.created_at {dir}, u.id {dir}"),
        Some(_) => return Err(invalid("Invalid sort")),
    };
    let mut count_q: QueryBuilder<Postgres> =
        QueryBuilder::new("SELECT count(*) FROM users u WHERE ");
    where_sql.build(&mut count_q);
    let total: i64 = count_q.build_query_scalar().fetch_one(&state.db).await?;
    let joined = texts.join(" ");
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("SELECT ");
    qb.push(db::prefixed("u", db::User::COLUMNS));
    qb.push(", (CASE WHEN lower(u.login) = ")
        .push_bind(joined.clone())
        .push(" THEN 2 ELSE 0 END + similarity(lower(u.login), ")
        .push_bind(joined)
        .push("))::real AS score FROM users u WHERE ");
    where_sql.build(&mut qb);
    qb.push(format!(" ORDER BY {order} LIMIT "))
        .push_bind(page_limit(&p))
        .push(" OFFSET ")
        .push_bind(p.offset());
    let hits: Vec<Hit> = qb.build_query_as().fetch_all(&state.db).await?;
    let matches = wants_text_matches(&headers);
    let needles: Vec<&str> = texts.iter().map(String::as_str).collect();
    let items = hits
        .into_iter()
        .map(|h| {
            let item = SimpleUser::new(&state.urls, &h.user);
            let text_matches = matches.then(|| {
                let mut v = Vec::new();
                v.extend(text_match(
                    &item.url,
                    "User",
                    "login",
                    &h.user.login,
                    &needles,
                    200,
                ));
                if let Some(n) = &h.user.name {
                    v.extend(text_match(&item.url, "User", "name", n, &needles, 200));
                }
                v
            });
            Scored {
                item,
                score: f64::from(h.score).max(if texts.is_empty() { 1.0 } else { 0.0 }),
                text_matches,
            }
        })
        .collect();
    Ok(SearchResult::new(&p, total, items, false))
}

#[derive(Debug, Default, Deserialize)]
pub struct LabelParams {
    pub repository_id: Option<i64>,
    pub q: Option<String>,
    pub sort: Option<String>,
    pub order: Option<String>,
}

#[derive(FromRow)]
struct LabelHit {
    #[sqlx(flatten)]
    label: db::Label,
    score: f32,
}

/// `GET /search/labels?repository_id=&q=`
pub async fn labels(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Query(params): Query<LabelParams>,
) -> ApiResult<SearchResult<Scored<Label>>> {
    let repo_id = params.repository_id.ok_or_else(|| {
        ApiError::invalid_field(FieldError::new("Search", "repository_id", "missing"))
    })?;
    let search = SearchParams {
        q: params.q.clone(),
        sort: params.sort.clone(),
        order: params.order.clone(),
    };
    let (_, q) = search.query()?;
    check_window(&p)?;
    let repo = db::Repository::find(&state.db, repo_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner = db::User::find(&state.db, repo.owner_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let access = RepoAccess::for_repo(&state, auth.as_ref(), repo, owner).await?;
    let texts: Vec<String> = q.texts().iter().map(|t| t.to_lowercase()).collect();
    let mut c = Conds::default();
    c.with(|s| {
        s.raw("l.repo_id = ").i64(access.repo.id);
    });
    for t in &texts {
        let pat = format!("%{}%", like_escape(t));
        c.with(|s| {
            s.raw("lower(l.name) LIKE ")
                .text(pat.clone())
                .raw(" OR lower(coalesce(l.description, '')) LIKE ")
                .text(pat);
        });
    }
    let where_sql = c.to_sql();
    let dir = if search.ascending() { "ASC" } else { "DESC" };
    let order = match search.sort() {
        None | Some("best-match") => format!("score {dir}, lower(l.name), l.id"),
        Some("created") => format!("l.created_at {dir}, l.id {dir}"),
        Some("updated") => format!("l.updated_at {dir}, l.id {dir}"),
        Some(_) => return Err(invalid("Invalid sort")),
    };
    let mut count_q: QueryBuilder<Postgres> =
        QueryBuilder::new("SELECT count(*) FROM labels l WHERE ");
    where_sql.build(&mut count_q);
    let total: i64 = count_q.build_query_scalar().fetch_one(&state.db).await?;
    let joined = texts.join(" ");
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("SELECT ");
    qb.push(db::prefixed("l", db::Label::COLUMNS));
    qb.push(", (CASE WHEN lower(l.name) = ")
        .push_bind(joined.clone())
        .push(" THEN 2 ELSE 0 END + similarity(lower(l.name), ")
        .push_bind(joined)
        .push("))::real AS score FROM labels l WHERE ");
    where_sql.build(&mut qb);
    qb.push(format!(" ORDER BY {order} LIMIT "))
        .push_bind(page_limit(&p))
        .push(" OFFSET ")
        .push_bind(p.offset());
    let hits: Vec<LabelHit> = qb.build_query_as().fetch_all(&state.db).await?;
    let matches = wants_text_matches(&headers);
    let needles: Vec<&str> = texts.iter().map(String::as_str).collect();
    let items = hits
        .into_iter()
        .map(|h| {
            let item = Label::new(
                &state.urls,
                &access.owner.login,
                &access.repo.name,
                &h.label,
            );
            let text_matches = matches.then(|| {
                let mut v = Vec::new();
                v.extend(text_match(
                    &item.url, "Label", "name", &item.name, &needles, 200,
                ));
                if let Some(d) = &item.description {
                    v.extend(text_match(
                        &item.url,
                        "Label",
                        "description",
                        d,
                        &needles,
                        200,
                    ));
                }
                v
            });
            Scored {
                item,
                score: f64::from(h.score),
                text_matches,
            }
        })
        .collect();
    Ok(SearchResult::new(&p, total, items, false))
}
