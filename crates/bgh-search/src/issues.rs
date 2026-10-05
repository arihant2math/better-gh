//! `GET /search/issues`: issues and pull requests with GitHub qualifiers,
//! backed by Postgres full-text search (`issues.search`, comment bodies).

use axum::extract::State;
use axum::http::HeaderMap;
use bgh_core::perms::{self, ReadableRepos};
use bgh_core::prelude::*;
use sqlx::{FromRow, Postgres, QueryBuilder};

use crate::common::{
    self, Scored, SearchParams, SearchResult, check_window, page_limit, text_match,
};
use crate::query::{DateRange, NumRange, Query as SearchQuery, Term, comma_list};
use crate::render::{IssueContext, IssueJson};
use crate::resolve::{self, Users};
use crate::sqlb::{Arg, Conds, Sql, date_range, num_range};

/// Rebuild the free-text part as `websearch_to_tsquery` input.
pub fn websearch_text(q: &SearchQuery) -> String {
    let mut parts = Vec::new();
    for t in &q.terms {
        match t {
            Term::Text {
                text,
                quoted,
                negated,
            } => {
                let clean = text.replace('"', " ");
                let mut s = String::new();
                if *negated {
                    s.push('-');
                }
                if *quoted {
                    s.push('"');
                    s.push_str(&clean);
                    s.push('"');
                } else {
                    s.push_str(&clean);
                }
                parts.push(s);
            }
            Term::Or => parts.push("or".into()),
            _ => {}
        }
    }
    parts.join(" ")
}

fn invalid(msg: &str) -> ApiError {
    ApiError::invalid_field(FieldError {
        message: Some(msg.to_string()),
        ..FieldError::new("Search", "q", "invalid")
    })
}

/// Logins referenced by user-valued qualifiers.
const USER_QUALS: &[&str] = &[
    "author",
    "assignee",
    "commenter",
    "involves",
    "user",
    "org",
    "review-requested",
    "reviewed-by",
];

/// Build the WHERE clause (aliases: `i` issues, `r` repositories, `pr`
/// pull_requests LEFT JOIN). Returns the conditions and the tsquery text.
pub async fn filters(
    state: &AppState,
    auth: Option<&AuthContext>,
    readable: &ReadableRepos,
    q: &SearchQuery,
) -> ApiResult<(Conds, Option<String>)> {
    let mut logins = Vec::new();
    let mut repo_names = Vec::new();
    for t in &q.terms {
        if let Some((k, v, _)) = t.qualifier() {
            if USER_QUALS.contains(&k) {
                logins.extend(comma_list(v));
            } else if k == "repo" {
                repo_names.extend(comma_list(v));
            }
        }
    }
    let users = Users::load(state, auth, logins).await?;
    let repos = resolve::repos(state, readable, repo_names).await?;

    let mut c = Conds::default();
    c.push(crate::sqlb::readable(readable, "r"));

    let text = websearch_text(q);
    let has_text = q.terms.iter().any(|t| matches!(t, Term::Text { .. }));

    // in:title,body,comments (default all)
    let mut in_title = false;
    let mut in_body = false;
    let mut in_comments = false;
    for (v, _) in q.quals("in") {
        for f in comma_list(v) {
            match f.to_lowercase().as_str() {
                "title" => in_title = true,
                "body" => in_body = true,
                "comments" | "comment" => in_comments = true,
                _ => {}
            }
        }
    }
    if !(in_title || in_body || in_comments) {
        (in_title, in_body, in_comments) = (true, true, true);
    }
    if has_text {
        c.with(|s| {
            let mut first = true;
            let mut or = |s: &mut Sql| {
                if !first {
                    s.raw(" OR ");
                }
                first = false;
            };
            if in_title && in_body {
                or(s);
                s.raw("i.search @@ websearch_to_tsquery('english', ")
                    .text(text.clone())
                    .raw(")");
            } else if in_title {
                or(s);
                s.raw("to_tsvector('english', i.title) @@ websearch_to_tsquery('english', ")
                    .text(text.clone())
                    .raw(")");
            } else if in_body {
                or(s);
                s.raw("to_tsvector('english', coalesce(i.body, '')) @@ websearch_to_tsquery('english', ")
                    .text(text.clone())
                    .raw(")");
            }
            if in_comments {
                or(s);
                s.raw("i.id IN (SELECT c.issue_id FROM comments c WHERE to_tsvector('english', c.body) @@ websearch_to_tsquery('english', ")
                    .text(text.clone())
                    .raw("))");
            }
        });
    }

    // Multiple repo:/user:/org: qualifiers are OR-ed together.
    let mut scope = Sql::new();
    let mut scope_n = 0;
    for t in &q.terms {
        let Some((k, v, neg)) = t.qualifier() else {
            continue;
        };
        if neg || !matches!(k, "repo" | "user" | "org") {
            continue;
        }
        if scope_n > 0 {
            scope.raw(" OR ");
        }
        scope_n += 1;
        match k {
            "repo" => {
                let ids: Vec<i64> = comma_list(v)
                    .iter()
                    .map(|n| repos.get(&n.to_lowercase()).copied().unwrap_or(0))
                    .collect();
                scope.raw("i.repo_id = ANY(").arg(Arg::I64s(ids)).raw(")");
            }
            _ => {
                scope
                    .raw("r.owner_id = ANY(")
                    .arg(Arg::I64s(users.ids(&comma_list(v))))
                    .raw(")");
            }
        }
    }
    c.push(scope);

    for t in &q.terms {
        let Some((key, value, neg)) = t.qualifier() else {
            continue;
        };
        let mut s = Sql::new();
        let not = if neg { "NOT " } else { "" };
        match key {
            "type" | "is" => match value.to_lowercase().as_str() {
                "issue" => {
                    s.raw(format!("{not}NOT i.is_pull_request"));
                }
                "pr" | "pull-request" | "pullrequest" => {
                    s.raw(format!("{not}i.is_pull_request"));
                }
                "open" | "closed" => {
                    s.raw(format!("{not}i.state = ")).text(value.to_lowercase());
                }
                "merged" => {
                    s.raw(format!("{not}coalesce(pr.merged, false)"));
                }
                "unmerged" => {
                    s.raw(format!(
                        "{not}(i.is_pull_request AND i.state = 'closed' AND NOT coalesce(pr.merged, false))"
                    ));
                }
                "draft" => {
                    s.raw(format!("{not}coalesce(pr.draft, false)"));
                }
                "locked" => {
                    s.raw(format!("{not}i.locked"));
                }
                "unlocked" => {
                    s.raw(format!("{not}NOT i.locked"));
                }
                "public" => {
                    s.raw(format!("{not}r.visibility = 'public'"));
                }
                "private" | "internal" => {
                    s.raw(format!("{not}r.visibility <> 'public'"));
                }
                "archived" => {
                    s.raw(format!("{not}r.archived"));
                }
                _ => {
                    return Err(invalid(&format!(
                        "Invalid value {value:?} for is: qualifier"
                    )));
                }
            },
            "state" => match value.to_lowercase().as_str() {
                v @ ("open" | "closed") => {
                    s.raw(format!("{not}i.state = ")).text(v);
                }
                _ => return Err(invalid("state must be open or closed")),
            },
            "reason" => {
                let r = match value.to_lowercase().replace(['-', '_'], " ").as_str() {
                    "completed" => "completed",
                    "not planned" => "not_planned",
                    "reopened" => "reopened",
                    "duplicate" => "duplicate",
                    _ => return Err(invalid("Invalid reason")),
                };
                s.raw(format!("{not}coalesce(i.state_reason = "))
                    .text(r)
                    .raw(", false)");
            }
            "author" => {
                s.raw(format!("{not}coalesce(i.author_id = ANY("))
                    .arg(Arg::I64s(users.ids(&comma_list(value))))
                    .raw("), false)");
            }
            "assignee" => {
                s.raw(format!(
                    "{not}EXISTS (SELECT 1 FROM issue_assignees a WHERE a.issue_id = i.id AND a.user_id = ANY("
                ))
                .arg(Arg::I64s(users.ids(&comma_list(value))))
                .raw("))");
            }
            "commenter" => {
                s.raw(format!(
                    "{not}i.id IN (SELECT c.issue_id FROM comments c WHERE c.author_id = ANY("
                ))
                .arg(Arg::I64s(users.ids(&comma_list(value))))
                .raw("))");
            }
            "mentions" => {
                let login: String = value
                    .trim_start_matches('@')
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                mentions(&mut s, not, &login);
            }
            "involves" => {
                let ids = users.ids(&comma_list(value));
                s.raw(format!("{not}(coalesce(i.author_id = ANY("))
                    .arg(Arg::I64s(ids.clone()))
                    .raw("), false) OR EXISTS (SELECT 1 FROM issue_assignees a WHERE a.issue_id = i.id AND a.user_id = ANY(")
                    .arg(Arg::I64s(ids.clone()))
                    .raw(")) OR i.id IN (SELECT c.issue_id FROM comments c WHERE c.author_id = ANY(")
                    .arg(Arg::I64s(ids))
                    .raw(")))");
            }
            "review-requested" => {
                s.raw(format!(
                    "{not}EXISTS (SELECT 1 FROM pr_requested_reviewers rr WHERE rr.pull_id = i.id AND rr.user_id = ANY("
                ))
                .arg(Arg::I64s(users.ids(&comma_list(value))))
                .raw("))");
            }
            "reviewed-by" => {
                s.raw(format!(
                    "{not}EXISTS (SELECT 1 FROM pr_reviews rv WHERE rv.pull_id = i.id AND rv.user_id = ANY("
                ))
                .arg(Arg::I64s(users.ids(&comma_list(value))))
                .raw("))");
            }
            "label" => {
                let names: Vec<String> =
                    comma_list(value).iter().map(|l| l.to_lowercase()).collect();
                s.raw(format!(
                    "{not}EXISTS (SELECT 1 FROM issue_labels il JOIN labels l ON l.id = il.label_id
                       WHERE il.issue_id = i.id AND lower(l.name) = ANY("
                ))
                .arg(Arg::Texts(names))
                .raw("))");
            }
            "milestone" => {
                s.raw(format!(
                    "{not}coalesce(i.milestone_id IN (SELECT m.id FROM milestones m WHERE m.repo_id = i.repo_id AND lower(m.title) = "
                ))
                .text(value.to_lowercase())
                .raw("), false)");
            }
            "no" => {
                let cond = match value.to_lowercase().as_str() {
                    "label" => {
                        "NOT EXISTS (SELECT 1 FROM issue_labels il WHERE il.issue_id = i.id)"
                    }
                    "milestone" => "i.milestone_id IS NULL",
                    "assignee" => {
                        "NOT EXISTS (SELECT 1 FROM issue_assignees a WHERE a.issue_id = i.id)"
                    }
                    "project" => "TRUE",
                    _ => return Err(invalid("Invalid value for no: qualifier")),
                };
                s.raw(format!("{not}({cond})"));
            }
            "created" | "updated" | "closed" | "merged" => {
                let range = DateRange::parse(value)
                    .ok_or_else(|| invalid(&format!("Invalid date for {key}: qualifier")))?;
                let expr = match key {
                    "created" => "i.created_at",
                    "updated" => "i.updated_at",
                    "closed" => "i.closed_at",
                    _ => "pr.merged_at",
                };
                s.raw(format!("{not}coalesce(("));
                date_range(&mut s, expr, &range);
                s.raw("), false)");
            }
            "comments" => {
                let range =
                    NumRange::parse(value).ok_or_else(|| invalid("Invalid comments: range"))?;
                s.raw(format!("{not}("));
                num_range(&mut s, "i.comments_count", &range);
                s.raw(")");
            }
            "reactions" | "interactions" => {
                let range =
                    NumRange::parse(value).ok_or_else(|| invalid("Invalid reactions: range"))?;
                let expr = if key == "reactions" {
                    REACTIONS_EXPR.to_string()
                } else {
                    format!("(i.comments_count + {REACTIONS_EXPR})")
                };
                s.raw(format!("{not}("));
                num_range(&mut s, &expr, &range);
                s.raw(")");
            }
            "head" | "base" => {
                let col = if key == "head" {
                    "pr.head_ref"
                } else {
                    "pr.base_ref"
                };
                s.raw(format!("{not}coalesce({col} = "))
                    .text(value)
                    .raw(", false)");
            }
            "draft" => {
                let v = value.eq_ignore_ascii_case("true");
                s.raw(format!("{not}(i.is_pull_request AND pr.draft = "))
                    .arg(Arg::Bool(v))
                    .raw(")");
            }
            "archived" => {
                let v = value.eq_ignore_ascii_case("true");
                s.raw(format!("{not}r.archived = ")).arg(Arg::Bool(v));
            }
            "language" => {
                s.raw(format!("{not}coalesce(lower(r.language) = "))
                    .text(value.to_lowercase())
                    .raw(", false)");
            }
            "repo" | "user" | "org" if neg => match key {
                "repo" => {
                    let ids: Vec<i64> = comma_list(value)
                        .iter()
                        .map(|n| repos.get(&n.to_lowercase()).copied().unwrap_or(0))
                        .collect();
                    s.raw("NOT i.repo_id = ANY(").arg(Arg::I64s(ids)).raw(")");
                }
                _ => {
                    s.raw("NOT r.owner_id = ANY(")
                        .arg(Arg::I64s(users.ids(&comma_list(value))))
                        .raw(")");
                }
            },
            // Positive repo/user/org handled above; unknown qualifiers ignored.
            _ => {}
        }
        c.push(s);
    }
    Ok((c, has_text.then_some(text)))
}

fn mentions(s: &mut Sql, not: &str, login: &str) {
    let pattern = format!("(^|[^A-Za-z0-9-])@{login}([^A-Za-z0-9-]|$)");
    s.raw(format!("{not}(coalesce(i.body ~* "))
        .text(pattern.clone())
        .raw(", false) OR i.id IN (SELECT c.issue_id FROM comments c WHERE c.body ~* ")
        .text(pattern)
        .raw("))");
}

const REACTIONS_EXPR: &str =
    "(SELECT count(*) FROM reactions x WHERE x.subject_type = 'issue' AND x.subject_id = i.id)";

fn order_by(params: &SearchParams, has_text: bool) -> ApiResult<String> {
    let dir = if params.ascending() { "ASC" } else { "DESC" };
    let expr = match params.sort() {
        None | Some("best-match") => {
            return Ok(if has_text {
                format!("score {dir}, i.updated_at DESC, i.id DESC")
            } else {
                format!("i.created_at {dir}, i.id {dir}")
            });
        }
        Some("created") => "i.created_at".to_string(),
        Some("updated") => "i.updated_at".to_string(),
        Some("comments") => "i.comments_count".to_string(),
        Some("reactions") => REACTIONS_EXPR.to_string(),
        Some("interactions") => format!("(i.comments_count + {REACTIONS_EXPR})"),
        Some(s) if s.starts_with("reactions-") => {
            let content = match &s["reactions-".len()..] {
                "+1" | " 1" => "+1",
                "-1" => "-1",
                "smile" | "laugh" => "laugh",
                "tada" | "hooray" => "hooray",
                c @ ("thinking_face" | "confused") => {
                    if c == "thinking_face" {
                        "confused"
                    } else {
                        c
                    }
                }
                "heart" => "heart",
                "rocket" => "rocket",
                "eyes" => "eyes",
                _ => return Err(invalid("Invalid sort")),
            };
            format!(
                "(SELECT count(*) FROM reactions x WHERE x.subject_type = 'issue' AND x.subject_id = i.id AND x.content = '{content}')"
            )
        }
        Some(_) => return Err(invalid("Invalid sort")),
    };
    Ok(format!("{expr} {dir}, i.id {dir}"))
}

#[derive(FromRow)]
struct Hit {
    #[sqlx(flatten)]
    issue: db::Issue,
    score: f32,
}

pub const FROM: &str = " FROM issues i JOIN repositories r ON r.id = i.repo_id \
    LEFT JOIN pull_requests pr ON pr.issue_id = i.id WHERE ";

/// `GET /search/issues`
pub async fn search(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Query(params): Query<SearchParams>,
) -> ApiResult<SearchResult<Scored<IssueJson>>> {
    let (_, q) = params.query()?;
    check_window(&p)?;
    let readable = perms::readable_repos(&state.db, auth.as_ref()).await?;
    let (conds, text) = filters(&state, auth.as_ref(), &readable, &q).await?;
    let where_sql = conds.to_sql();
    let order = order_by(&params, text.is_some())?;

    let mut count_q: QueryBuilder<Postgres> = QueryBuilder::new("SELECT count(*)");
    count_q.push(FROM);
    where_sql.build(&mut count_q);
    let total: i64 = count_q.build_query_scalar().fetch_one(&state.db).await?;

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new("SELECT ");
    qb.push(db::prefixed("i", db::Issue::COLUMNS));
    match &text {
        Some(t) => {
            qb.push(", ts_rank_cd(i.search, websearch_to_tsquery('english', ")
                .push_bind(t.clone())
                .push("))::real AS score");
        }
        None => {
            qb.push(", 1::real AS score");
        }
    }
    qb.push(FROM);
    where_sql.build(&mut qb);
    qb.push(format!(" ORDER BY {order} LIMIT "))
        .push_bind(page_limit(&p))
        .push(" OFFSET ")
        .push_bind(p.offset());
    let hits: Vec<Hit> = qb.build_query_as().fetch_all(&state.db).await?;

    let issues: Vec<db::Issue> = hits.iter().map(|h| h.issue.clone()).collect();
    let ctx = IssueContext::load(&state, &issues).await?;
    let matches = common::wants_text_matches(&headers);
    let needles = q.texts();
    let items = hits
        .iter()
        .filter_map(|h| {
            let json = ctx.issue(&state, &h.issue)?;
            let text_matches = matches.then(|| {
                let mut v = Vec::new();
                v.extend(text_match(
                    &json.url,
                    "Issue",
                    "title",
                    &json.title,
                    &needles,
                    300,
                ));
                if let Some(b) = &json.body {
                    v.extend(text_match(&json.url, "Issue", "body", b, &needles, 300));
                }
                v
            });
            Some(Scored {
                item: json,
                score: if text.is_some() {
                    f64::from(h.score)
                } else {
                    1.0
                },
                text_matches,
            })
        })
        .collect();
    Ok(SearchResult::new(&p, total, items, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse;

    #[test]
    fn websearch_input() {
        assert_eq!(
            websearch_text(&parse(r#"fix "null pointer" -flaky label:bug OR crash"#)),
            r#"fix "null pointer" -flaky or crash"#
        );
    }
}
