//! A tiny owned SQL fragment builder: search filters are assembled once and
//! emitted into several `sqlx::QueryBuilder`s (count + page queries) with
//! every value bound, never formatted.

use chrono::{DateTime, Utc};
use sqlx::{Postgres, QueryBuilder};

#[derive(Debug, Clone)]
pub enum Arg {
    I64(i64),
    Text(String),
    Ts(DateTime<Utc>),
    I64s(Vec<i64>),
    Texts(Vec<String>),
    Bool(bool),
}

#[derive(Debug, Clone)]
enum Part {
    Raw(String),
    Arg(Arg),
}

#[derive(Debug, Clone, Default)]
pub struct Sql {
    parts: Vec<Part>,
}

impl Sql {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn raw(&mut self, s: impl Into<String>) -> &mut Self {
        self.parts.push(Part::Raw(s.into()));
        self
    }

    pub fn arg(&mut self, a: Arg) -> &mut Self {
        self.parts.push(Part::Arg(a));
        self
    }

    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.arg(Arg::I64(v))
    }

    pub fn text(&mut self, v: impl Into<String>) -> &mut Self {
        self.arg(Arg::Text(v.into()))
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn append(&mut self, other: &Sql) -> &mut Self {
        self.parts.extend(other.parts.iter().cloned());
        self
    }

    /// Emit into a query builder.
    pub fn build<'a>(&self, qb: &mut QueryBuilder<'a, Postgres>) {
        for p in &self.parts {
            match p {
                Part::Raw(s) => {
                    qb.push(s);
                }
                Part::Arg(a) => match a.clone() {
                    Arg::I64(v) => {
                        qb.push_bind(v);
                    }
                    Arg::Text(v) => {
                        qb.push_bind(v);
                    }
                    Arg::Ts(v) => {
                        qb.push_bind(v);
                    }
                    Arg::I64s(v) => {
                        qb.push_bind(v);
                    }
                    Arg::Texts(v) => {
                        qb.push_bind(v);
                    }
                    Arg::Bool(v) => {
                        qb.push_bind(v);
                    }
                },
            }
        }
    }
}

/// `AND`-joined list of conditions.
#[derive(Debug, Clone, Default)]
pub struct Conds {
    conds: Vec<Sql>,
}

impl Conds {
    pub fn push(&mut self, s: Sql) {
        if !s.is_empty() {
            self.conds.push(s);
        }
    }

    /// Add a condition built by `f`.
    pub fn with(&mut self, f: impl FnOnce(&mut Sql)) {
        let mut s = Sql::new();
        f(&mut s);
        self.push(s);
    }

    pub fn to_sql(&self) -> Sql {
        let mut out = Sql::new();
        if self.conds.is_empty() {
            out.raw("TRUE");
        }
        for (i, c) in self.conds.iter().enumerate() {
            if i > 0 {
                out.raw(" AND ");
            }
            out.raw("(").append(c).raw(")");
        }
        out
    }
}

/// Append range conditions on `expr` for a date range.
pub fn date_range(s: &mut Sql, expr: &str, r: &crate::query::DateRange) {
    let mut first = true;
    if let Some(from) = r.from {
        s.raw(format!("{expr} >= ")).arg(Arg::Ts(from));
        first = false;
    }
    if let Some(to) = r.to {
        if !first {
            s.raw(" AND ");
        }
        s.raw(format!("{expr} < ")).arg(Arg::Ts(to));
        first = false;
    }
    if first {
        s.raw("TRUE");
    }
}

/// Append range conditions on `expr` for a numeric range.
pub fn num_range(s: &mut Sql, expr: &str, r: &crate::query::NumRange) {
    let mut first = true;
    if let Some(lo) = r.lo {
        s.raw(format!("{expr} {} ", if lo.inclusive { ">=" } else { ">" }))
            .i64(lo.value);
        first = false;
    }
    if let Some(hi) = r.hi {
        if !first {
            s.raw(" AND ");
        }
        s.raw(format!("{expr} {} ", if hi.inclusive { "<=" } else { "<" }))
            .i64(hi.value);
        first = false;
    }
    if first {
        s.raw("TRUE");
    }
}

/// Readability condition over the repositories alias `alias`.
pub fn readable(r: &bgh_core::perms::ReadableRepos, alias: &str) -> Sql {
    let mut s = Sql::new();
    if r.all {
        s.raw("TRUE");
    } else {
        s.raw(format!("{} OR {alias}.id = ANY(", r.visibility_sql(alias)))
            .arg(Arg::I64s(r.private_ids.clone()))
            .raw(")");
    }
    s
}
