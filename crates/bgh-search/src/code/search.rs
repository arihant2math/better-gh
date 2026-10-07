//! `GET /search/code`: substring and regex search over the trigram code
//! index, limited to repositories the caller can read.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::HeaderMap;
use bgh_core::models::api::MinimalRepository;
use bgh_core::perms;
use bgh_core::prelude::*;
use bgh_core::urls::encode_path;
use serde::Serialize;
use sqlx::{FromRow, Postgres, QueryBuilder};

use crate::common::{
    MAX_RESULTS, Scored, SearchParams, SearchResult, TextMatch, build_fragment, check_window,
    page_limit, wants_text_matches,
};
use crate::query::{NumRange, Term, comma_list, like_escape};
use crate::resolve::{self, Users};
use crate::sqlb::{Arg, Conds, Sql, num_range, readable};

/// Per-query time budget; slower queries return `incomplete_results`.
const TIMEOUT_MS: i64 = 10_000;

fn invalid(msg: &str) -> ApiError {
    ApiError::invalid_field(FieldError {
        message: Some(msg.to_string()),
        ..FieldError::new("Search", "q", "invalid")
    })
}

/// `code-search-result-item`.
#[derive(Debug, Clone, Serialize)]
pub struct CodeItem {
    pub name: String,
    pub path: String,
    pub sha: String,
    pub url: String,
    pub git_url: String,
    pub html_url: String,
    pub repository: MinimalRepository,
    pub file_size: i64,
    pub language: Option<String>,
    pub line_numbers: Vec<String>,
}

#[derive(FromRow)]
struct Hit {
    repo_id: i64,
    path: String,
    blob_sha: String,
    name: String,
    language: Option<String>,
    size: i32,
    commit_sha: String,
    content: Option<String>,
}

/// A content matcher used for SQL and for highlighting.
enum Matcher {
    Sub(String),
    Re(regex::Regex),
}

impl Matcher {
    fn find_all(&self, text: &str) -> Vec<(usize, usize)> {
        match self {
            Matcher::Sub(n) => {
                let lower = text.to_lowercase();
                if lower.len() != text.len() {
                    return text
                        .match_indices(n.as_str())
                        .map(|(s, m)| (s, s + m.len()))
                        .collect();
                }
                let n = n.to_lowercase();
                lower
                    .match_indices(&n)
                    .map(|(s, m)| (s, s + m.len()))
                    .collect()
            }
            Matcher::Re(r) => r
                .find_iter(text)
                .filter(|m| !m.is_empty())
                .map(|m| (m.start(), m.end()))
                .collect(),
        }
    }
}

fn compile_regex(pattern: &str) -> ApiResult<regex::Regex> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
        .map_err(|_| invalid("Invalid regular expression"))
}

/// The content side of one term, written so `code_blobs_content_trgm_idx`
/// drives it (#277): wrapping `b.content` in `coalesce()` hides it from the
/// index and turns every search into a probe per file.
///
/// * positive, content only: plain `b.content <op> $1`. The term sits under
///   AND/OR only, so the NULL of an unindexed blob (LEFT JOIN) filters the
///   row out exactly as `coalesce(…, false)` did.
/// * positive, OR-ed with the path: `b.content <op> $1 OR <path>` spans two
///   tables, so nothing can drive it; a hashed sub-select of the matching
///   blob SHAs uses the content index and costs one hash probe per file.
/// * negated: `coalesce` stays so unindexed blobs still match `NOT`; no
///   index helps a negation anyway.
fn content_cond(s: &mut Sql, op: &str, value: String, negated: bool, with_path: bool) {
    if negated {
        s.raw(format!("coalesce(b.content {op} "))
            .text(value)
            .raw(", false)");
    } else if with_path {
        s.raw(format!(
            "f.blob_sha IN (SELECT sha FROM code_blobs WHERE content {op} "
        ))
        .text(value)
        .raw(")");
    } else {
        s.raw(format!("b.content {op} ")).text(value);
    }
}

/// One content/path predicate.
fn term_cond(s: &mut Sql, t: &Term, in_path: bool, in_file: bool) -> ApiResult<Option<Matcher>> {
    match t {
        Term::Text { text, negated, .. } => {
            let pat = format!("%{}%", like_escape(text));
            let not = if *negated { "NOT " } else { "" };
            s.raw(format!("{not}("));
            if in_file {
                content_cond(s, "ILIKE", pat.clone(), *negated, in_path);
            }
            if in_path {
                if in_file {
                    s.raw(" OR ");
                }
                s.raw("lower(f.path) LIKE ").text(pat.to_lowercase());
            }
            s.raw(")");
            Ok((!negated).then(|| Matcher::Sub(text.clone())))
        }
        Term::Regex { pattern, negated } => {
            let re = compile_regex(pattern)?;
            let not = if *negated { "NOT " } else { "" };
            s.raw(format!("{not}("));
            if in_file {
                content_cond(s, "~*", pattern.clone(), *negated, in_path);
            }
            if in_path {
                if in_file {
                    s.raw(" OR ");
                }
                s.raw("f.path ~* ").text(pattern.clone());
            }
            s.raw(")");
            Ok((!negated).then_some(Matcher::Re(re)))
        }
        _ => Ok(None),
    }
}

/// Convert a `path:` value to a LIKE pattern: globs (`*`, `**`) or a
/// case-insensitive substring; a leading `/` anchors at the root.
fn path_pattern(v: &str) -> String {
    let anchored = v.starts_with('/');
    let v = v.trim_start_matches('/');
    let mut out = String::new();
    let mut wild = false;
    if !anchored {
        out.push('%');
        wild = true;
    }
    for c in v.chars() {
        match c {
            '*' => {
                if !wild {
                    out.push('%');
                }
                wild = true;
                continue;
            }
            '?' => out.push('_'),
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c => out.extend(c.to_lowercase()),
        }
        wild = false;
    }
    if !v.contains('*') && !wild {
        out.push('%');
    }
    out
}

/// `GET /search/code`
pub async fn search(
    State(state): State<AppState>,
    auth: MaybeUser,
    headers: HeaderMap,
    p: Pagination,
    Query(params): Query<SearchParams>,
) -> ApiResult<SearchResult<Scored<CodeItem>>> {
    let (_, q) = params.query()?;
    check_window(&p)?;
    if params.sort().is_some_and(|s| s != "indexed") {
        return Err(invalid("Invalid sort"));
    }
    let readable_set = perms::readable_repos(&state.db, auth.as_ref()).await?;

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
    let users = Users::load(&state, auth.as_ref(), logins).await?;
    let repos = resolve::repos(&state, &readable_set, repo_names).await?;

    let mut in_path = false;
    let mut in_file = false;
    for (v, _) in q.quals("in") {
        for f in comma_list(v) {
            match f.to_lowercase().as_str() {
                "path" => in_path = true,
                "file" => in_file = true,
                _ => {}
            }
        }
    }
    if !in_path && !in_file {
        in_file = true;
    }

    let mut c = Conds::default();
    c.push(readable(&readable_set, "r"));
    // Free text / regex: AND within groups, OR between groups.
    let mut matchers = Vec::new();
    let mut groups: Vec<Sql> = Vec::new();
    let mut current = Sql::new();
    let mut current_n = 0;
    for t in &q.terms {
        match t {
            Term::Or => {
                if current_n > 0 {
                    groups.push(std::mem::take(&mut current));
                    current_n = 0;
                }
            }
            Term::Text { .. } | Term::Regex { .. } => {
                if current_n > 0 {
                    current.raw(" AND ");
                }
                current_n += 1;
                if let Some(m) = term_cond(&mut current, t, in_path, in_file)? {
                    matchers.push(m);
                }
            }
            _ => {}
        }
    }
    if current_n > 0 {
        groups.push(current);
    }
    if !groups.is_empty() {
        c.with(|s| {
            for (i, g) in groups.iter().enumerate() {
                if i > 0 {
                    s.raw(" OR ");
                }
                s.raw("(").append(g).raw(")");
            }
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
                    scope.raw("f.repo_id = ANY(").arg(Arg::I64s(ids)).raw(")");
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
                s.raw("NOT f.repo_id = ANY(").arg(Arg::I64s(ids)).raw(")");
            }
            "user" | "org" => {
                s.raw("NOT r.owner_id = ANY(")
                    .arg(Arg::I64s(users.ids(&comma_list(value))))
                    .raw(")");
            }
            "language" | "lang" => {
                s.raw(format!("{not}coalesce(lower(f.language) = ANY("))
                    .arg(Arg::Texts(
                        comma_list(value).iter().map(|l| l.to_lowercase()).collect(),
                    ))
                    .raw("), false)");
            }
            "extension" | "ext" => {
                s.raw(format!("{not}coalesce(f.extension = "))
                    .text(value.trim_start_matches('.').to_lowercase())
                    .raw(", false)");
            }
            "filename" => {
                s.raw(format!("{not}lower(f.name) LIKE "))
                    .text(path_pattern(value).trim_start_matches('%').to_string());
            }
            "path" => {
                if value.starts_with('/') && value.ends_with('/') && value.len() > 2 {
                    let re = &value[1..value.len() - 1];
                    compile_regex(re)?;
                    s.raw(format!("{not}f.path ~* ")).text(re);
                } else {
                    s.raw(format!("{not}lower(f.path) LIKE "))
                        .text(path_pattern(value));
                }
            }
            "size" => {
                let r = NumRange::parse(value).ok_or_else(|| invalid("Invalid size: range"))?;
                s.raw(format!("{not}("));
                num_range(&mut s, "f.size", &r);
                s.raw(")");
            }
            "fork" => {
                if !value.eq_ignore_ascii_case("true") && !value.eq_ignore_ascii_case("only") {
                    s.raw("NOT r.fork");
                } else if value.eq_ignore_ascii_case("only") {
                    s.raw("r.fork");
                }
            }
            "is" => match value.to_lowercase().as_str() {
                "archived" => {
                    s.raw(format!("{not}r.archived"));
                }
                "fork" => {
                    s.raw(format!("{not}r.fork"));
                }
                "public" => {
                    s.raw(format!("{not}r.visibility = 'public'"));
                }
                "private" => {
                    s.raw(format!("{not}r.visibility <> 'public'"));
                }
                "internal" => {
                    s.raw(format!("{not}r.visibility = 'internal'"));
                }
                _ => return Err(invalid("Invalid value for is: qualifier")),
            },
            _ => {}
        }
        c.push(s);
    }
    c.push(scope);
    let where_sql = c.to_sql();
    let with_text = wants_text_matches(&headers) && !matchers.is_empty();
    let from = " FROM code_files f JOIN repositories r ON r.id = f.repo_id \
        JOIN code_index_state st ON st.repo_id = f.repo_id \
        LEFT JOIN code_blobs b ON b.sha = f.blob_sha WHERE ";

    let mut tx = state.db.begin().await?;
    sqlx::query(&format!("SET LOCAL statement_timeout = {TIMEOUT_MS}"))
        .execute(&mut *tx)
        .await?;
    let mut count_q: QueryBuilder<Postgres> = QueryBuilder::new("SELECT count(*) FROM (SELECT 1");
    count_q.push(from);
    where_sql.build(&mut count_q);
    count_q.push(" LIMIT ").push_bind(MAX_RESULTS).push(") x");
    let total: Result<i64, sqlx::Error> = count_q.build_query_scalar().fetch_one(&mut *tx).await;
    let total = match total {
        Ok(t) => t,
        Err(e) if is_timeout(&e) => {
            return Ok(SearchResult::new(&p, 0, vec![], true));
        }
        Err(e) => return Err(e.into()),
    };

    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT f.repo_id, f.path, f.blob_sha, f.name, f.language, f.size, st.commit_sha, ",
    );
    qb.push(if with_text {
        "b.content"
    } else {
        "NULL::text AS content"
    });
    qb.push(from);
    where_sql.build(&mut qb);
    qb.push(" ORDER BY r.stargazers_count DESC, f.repo_id, f.path LIMIT ")
        .push_bind(page_limit(&p))
        .push(" OFFSET ")
        .push_bind(p.offset());
    let hits: Vec<Hit> = match qb.build_query_as().fetch_all(&mut *tx).await {
        Ok(h) => h,
        Err(e) if is_timeout(&e) => {
            return Ok(SearchResult::new(&p, total, vec![], true));
        }
        Err(e) => return Err(e.into()),
    };
    tx.commit().await?;

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

    let items = hits
        .into_iter()
        .filter_map(|h| {
            let repo = rendered.get(&h.repo_id)?.clone();
            let (owner, name) = repo.full_name.split_once('/')?;
            let path_enc = encode_path(&h.path);
            let url = state.urls.api(&format!(
                "/repos/{owner}/{name}/contents/{path_enc}?ref={}",
                h.commit_sha
            ));
            let (text_matches, line_numbers) = match (&h.content, with_text) {
                (Some(content), true) => {
                    let (m, lines) = content_matches(&url, content, &matchers);
                    (Some(m.into_iter().collect()), lines)
                }
                _ => (None, vec![]),
            };
            Some(Scored {
                item: CodeItem {
                    git_url: state
                        .urls
                        .api(&format!("/repos/{owner}/{name}/git/blobs/{}", h.blob_sha)),
                    html_url: state
                        .urls
                        .html(&format!("/{owner}/{name}/blob/{}/{path_enc}", h.commit_sha)),
                    url,
                    name: h.name,
                    path: h.path,
                    sha: h.blob_sha,
                    repository: repo,
                    file_size: i64::from(h.size),
                    language: h.language,
                    line_numbers,
                },
                score: 1.0,
                text_matches,
            })
        })
        .collect();
    Ok(SearchResult::new(&p, total, items, false))
}

fn is_timeout(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("57014"))
}

/// Text match fragment around the first hit, plus 1-based matching lines.
fn content_matches(
    url: &str,
    content: &str,
    matchers: &[Matcher],
) -> (Option<TextMatch>, Vec<String>) {
    let mut hits: Vec<(usize, usize)> = matchers.iter().flat_map(|m| m.find_all(content)).collect();
    hits.sort();
    let mut lines: Vec<String> = Vec::new();
    for (s, _) in hits.iter().take(20) {
        let line = content[..*s].matches('\n').count() + 1;
        let l = line.to_string();
        if !lines.contains(&l) {
            lines.push(l);
        }
    }
    (
        build_fragment(url, "FileContent", "content", content, hits, 400),
        lines,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cond(q: &str, in_path: bool, in_file: bool) -> String {
        let t = crate::query::parse(q).terms.remove(0);
        let mut s = Sql::new();
        term_cond(&mut s, &t, in_path, in_file).unwrap();
        s.render()
    }

    /// Positive content terms must reach `code_blobs_content_trgm_idx`; a
    /// `coalesce()` around `b.content` hides it (#277).
    #[test]
    fn content_terms_are_index_shaped() {
        assert_eq!(cond("foo", false, true), "(b.content ILIKE $1)");
        assert_eq!(cond("/fo+/", false, true), "(b.content ~* $1)");
        assert_eq!(
            cond("foo", true, true),
            "(f.blob_sha IN (SELECT sha FROM code_blobs WHERE content ILIKE $1) \
             OR lower(f.path) LIKE $2)"
        );
        assert_eq!(
            cond("/fo+/", true, true),
            "(f.blob_sha IN (SELECT sha FROM code_blobs WHERE content ~* $1) OR f.path ~* $2)"
        );
        assert_eq!(cond("foo", true, false), "(lower(f.path) LIKE $1)");
        // Negations keep coalesce: an unindexed blob matches NOT foo.
        assert_eq!(
            cond("-foo", false, true),
            "NOT (coalesce(b.content ILIKE $1, false))"
        );
        assert_eq!(
            cond("-foo", true, true),
            "NOT (coalesce(b.content ILIKE $1, false) OR lower(f.path) LIKE $2)"
        );
    }

    #[test]
    fn path_patterns() {
        assert_eq!(path_pattern("src/"), "%src/%");
        assert_eq!(path_pattern("/src"), "src%");
        assert_eq!(path_pattern("*.rs"), "%.rs");
        assert_eq!(path_pattern("/src/**/*.rs"), "src/%/%.rs");
        assert_eq!(path_pattern("a_b"), "%a\\_b%");
    }

    #[test]
    fn matchers() {
        let m = Matcher::Sub("Foo".into());
        assert_eq!(m.find_all("a foo FOO"), vec![(2, 5), (6, 9)]);
        let r = Matcher::Re(compile_regex("fo+").unwrap());
        assert_eq!(r.find_all("x fooo"), vec![(2, 6)]);
        assert!(compile_regex("(").is_err());
        let (m, lines) = content_matches(
            "u",
            "one\ntwo foo\nthree foo",
            &[Matcher::Sub("foo".into())],
        );
        assert_eq!(lines, vec!["2", "3"]);
        assert!(m.unwrap().fragment.starts_with("two foo"));
    }
}
