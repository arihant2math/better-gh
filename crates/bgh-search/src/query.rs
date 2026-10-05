//! GitHub search query syntax.
//!
//! `fix crash label:bug -label:wontfix "exact phrase" author:alice
//! created:>=2024-01-01 comments:5..10 NOT flaky /regex/ OR other`
//!
//! [`parse`] tokenizes into [`Term`]s; qualifier values are interpreted by
//! each search kind (ranges via [`NumRange`] / [`DateRange`]).

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// A bare word or quoted phrase.
    Text {
        text: String,
        quoted: bool,
        negated: bool,
    },
    /// `/pattern/` (code search).
    Regex { pattern: String, negated: bool },
    /// `key:value` (key lowercased, `-key:value` → negated).
    Qualifier {
        key: String,
        value: String,
        negated: bool,
    },
    /// The `OR` keyword.
    Or,
}

impl Term {
    pub fn qualifier(&self) -> Option<(&str, &str, bool)> {
        match self {
            Term::Qualifier {
                key,
                value,
                negated,
            } => Some((key, value, *negated)),
            _ => None,
        }
    }
}

/// Parsed query: terms in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub terms: Vec<Term>,
}

impl Query {
    /// Qualifier values for `key` (non-negated, negated).
    pub fn quals<'a>(&'a self, key: &'a str) -> impl Iterator<Item = (&'a str, bool)> + 'a {
        self.terms.iter().filter_map(move |t| match t {
            Term::Qualifier {
                key: k,
                value,
                negated,
            } if k == key => Some((value.as_str(), *negated)),
            _ => None,
        })
    }

    /// First non-negated value of `key`.
    pub fn qual<'a>(&'a self, key: &str) -> Option<&'a str> {
        self.terms.iter().find_map(|t| match t {
            Term::Qualifier {
                key: k,
                value,
                negated: false,
            } if k == key => Some(value.as_str()),
            _ => None,
        })
    }

    /// Positive free-text terms (words and phrases).
    pub fn texts(&self) -> Vec<&str> {
        self.terms
            .iter()
            .filter_map(|t| match t {
                Term::Text {
                    text,
                    negated: false,
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Negated free-text terms.
    pub fn negated_texts(&self) -> Vec<&str> {
        self.terms
            .iter()
            .filter_map(|t| match t {
                Term::Text {
                    text,
                    negated: true,
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// All positive text joined with spaces (for full-text queries).
    pub fn text(&self) -> String {
        self.texts().join(" ")
    }
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Tokenize a query string.
pub fn parse(input: &str) -> Query {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut terms = Vec::new();
    let mut pending_not = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() || c == '(' || c == ')' {
            i += 1;
            continue;
        }
        // Leading '-' negates (but not a bare '-').
        let mut negated = pending_not;
        pending_not = false;
        if c == '-' && i + 1 < chars.len() && !chars[i + 1].is_whitespace() {
            negated = !negated;
            i += 1;
        }
        let c = chars[i];
        if c == '"' {
            let (text, next) = read_quoted(&chars, i);
            i = next;
            if !text.is_empty() {
                terms.push(Term::Text {
                    text,
                    quoted: true,
                    negated,
                });
            }
            continue;
        }
        if c == '/' {
            // Regex up to the next unescaped '/'.
            let mut j = i + 1;
            let mut pat = String::new();
            let mut closed = false;
            while j < chars.len() {
                if chars[j] == '\\' && j + 1 < chars.len() && chars[j + 1] == '/' {
                    pat.push('/');
                    j += 2;
                    continue;
                }
                if chars[j] == '/' {
                    closed = true;
                    break;
                }
                pat.push(chars[j]);
                j += 1;
            }
            if closed && !pat.is_empty() {
                terms.push(Term::Regex {
                    pattern: pat,
                    negated,
                });
                i = j + 1;
                continue;
            }
        }
        // A word, possibly `key:value`.
        let start = i;
        while i < chars.len() && is_key_char(chars[i]) {
            i += 1;
        }
        if i > start && i < chars.len() && chars[i] == ':' {
            let key: String = chars[start..i].iter().collect::<String>().to_lowercase();
            i += 1;
            let value = if i < chars.len() && chars[i] == '"' {
                let (v, next) = read_quoted(&chars, i);
                i = next;
                v
            } else {
                let vs = i;
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                chars[vs..i].iter().collect()
            };
            terms.push(Term::Qualifier {
                key,
                value,
                negated,
            });
            continue;
        }
        // Plain word up to whitespace.
        i = start;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '(' && chars[i] != ')' {
            i += 1;
        }
        let word: String = chars[start..i].iter().collect();
        if word.is_empty() {
            i += 1;
            continue;
        }
        match word.as_str() {
            "OR" if !negated => terms.push(Term::Or),
            "AND" if !negated => {}
            "NOT" if !negated => pending_not = true,
            _ => terms.push(Term::Text {
                text: word,
                quoted: false,
                negated,
            }),
        }
    }
    Query { terms }
}

fn read_quoted(chars: &[char], start: usize) -> (String, usize) {
    let mut i = start + 1;
    let mut s = String::new();
    while i < chars.len() && chars[i] != '"' {
        if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1] == '"' {
            s.push('"');
            i += 2;
            continue;
        }
        s.push(chars[i]);
        i += 1;
    }
    (s, (i + 1).min(chars.len()))
}

/// A bound of a range: value and whether it is inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound<T> {
    pub value: T,
    pub inclusive: bool,
}

/// `n`, `>n`, `>=n`, `<n`, `<=n`, `a..b`, `a..*`, `*..b`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NumRange {
    pub lo: Option<Bound<i64>>,
    pub hi: Option<Bound<i64>>,
}

fn split_op(s: &str) -> (&str, &str) {
    for op in [">=", "<=", ">", "<"] {
        if let Some(rest) = s.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("", s)
}

impl NumRange {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some((a, b)) = s.split_once("..") {
            let lo = match a {
                "*" | "" => None,
                a => Some(Bound {
                    value: a.parse().ok()?,
                    inclusive: true,
                }),
            };
            let hi = match b {
                "*" | "" => None,
                b => Some(Bound {
                    value: b.parse().ok()?,
                    inclusive: true,
                }),
            };
            return Some(Self { lo, hi });
        }
        let (op, rest) = split_op(s);
        let v: i64 = rest.parse().ok()?;
        Some(match op {
            ">" => Self {
                lo: Some(Bound {
                    value: v,
                    inclusive: false,
                }),
                hi: None,
            },
            ">=" => Self {
                lo: Some(Bound {
                    value: v,
                    inclusive: true,
                }),
                hi: None,
            },
            "<" => Self {
                lo: None,
                hi: Some(Bound {
                    value: v,
                    inclusive: false,
                }),
            },
            "<=" => Self {
                lo: None,
                hi: Some(Bound {
                    value: v,
                    inclusive: true,
                }),
            },
            _ => Self {
                lo: Some(Bound {
                    value: v,
                    inclusive: true,
                }),
                hi: Some(Bound {
                    value: v,
                    inclusive: true,
                }),
            },
        })
    }
}

/// A half-open date range `[from, to)` (either side optional).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DateRange {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

/// Parse a date or datetime; returns (start, end-exclusive of its precision).
fn parse_instant(s: &str) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let start = Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?);
        return Some((start, start + Duration::days(1)));
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        let t = dt.with_timezone(&Utc);
        return Some((t, t + Duration::seconds(1)));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            let t = Utc.from_utc_datetime(&ndt);
            return Some((t, t + Duration::seconds(1)));
        }
    }
    None
}

impl DateRange {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some((a, b)) = s.split_once("..") {
            let from = match a {
                "*" | "" => None,
                a => Some(parse_instant(a)?.0),
            };
            let to = match b {
                "*" | "" => None,
                b => Some(parse_instant(b)?.1),
            };
            return Some(Self { from, to });
        }
        let (op, rest) = split_op(s);
        let (start, end) = parse_instant(rest)?;
        Some(match op {
            ">" => Self {
                from: Some(end),
                to: None,
            },
            ">=" => Self {
                from: Some(start),
                to: None,
            },
            "<" => Self {
                from: None,
                to: Some(start),
            },
            "<=" => Self {
                from: None,
                to: Some(end),
            },
            _ => Self {
                from: Some(start),
                to: Some(end),
            },
        })
    }
}

/// Split a comma list (`label:bug,wontfix` means either).
pub fn comma_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Escape `%`, `_` and `\` for a `LIKE` pattern.
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Words of `text` reduced to letters/digits, for building `to_tsquery`
/// input safely (`foo:* & bar:*`).
pub fn tsquery_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(key: &str, value: &str, negated: bool) -> Term {
        Term::Qualifier {
            key: key.into(),
            value: value.into(),
            negated,
        }
    }

    fn t(text: &str, quoted: bool, negated: bool) -> Term {
        Term::Text {
            text: text.into(),
            quoted,
            negated,
        }
    }

    #[test]
    fn tokenizes() {
        let parsed = parse(
            r#"fix crash label:bug -label:wontfix "exact phrase" Author:alice NOT flaky label:"good first issue" /fo+o/ OR x"#,
        );
        assert_eq!(
            parsed.terms,
            vec![
                t("fix", false, false),
                t("crash", false, false),
                q("label", "bug", false),
                q("label", "wontfix", true),
                t("exact phrase", true, false),
                q("author", "alice", false),
                t("flaky", false, true),
                q("label", "good first issue", false),
                Term::Regex {
                    pattern: "fo+o".into(),
                    negated: false
                },
                Term::Or,
                t("x", false, false),
            ]
        );
        assert_eq!(parsed.qual("author"), Some("alice"));
        assert_eq!(parsed.text(), "fix crash exact phrase x");
        assert_eq!(parsed.negated_texts(), vec!["flaky"]);
    }

    #[test]
    fn edge_cases() {
        assert_eq!(parse("").terms, vec![]);
        assert_eq!(parse("  -  ").terms, vec![t("-", false, false)]);
        assert_eq!(parse("repo:a/b").terms, vec![q("repo", "a/b", false)]);
        assert_eq!(
            parse("created:>=2024-01-01").terms,
            vec![q("created", ">=2024-01-01", false)]
        );
        assert_eq!(
            parse("url:http://x").terms,
            vec![q("url", "http://x", false)]
        );
        assert_eq!(parse("(a)").terms, vec![t("a", false, false)]);
        assert_eq!(
            parse("\"unterminated").terms,
            vec![t("unterminated", true, false)]
        );
        assert_eq!(parse("a/b").terms, vec![t("a/b", false, false)]);
    }

    #[test]
    fn ranges() {
        let r = NumRange::parse(">=5").unwrap();
        assert_eq!(
            r.lo,
            Some(Bound {
                value: 5,
                inclusive: true
            })
        );
        assert!(r.hi.is_none());
        let r = NumRange::parse("10..*").unwrap();
        assert_eq!(r.lo.unwrap().value, 10);
        let r = NumRange::parse("3").unwrap();
        assert_eq!((r.lo.unwrap().value, r.hi.unwrap().value), (3, 3));
        assert!(NumRange::parse("abc").is_none());

        let d = DateRange::parse("2024-01-02").unwrap();
        assert_eq!(d.from.unwrap().to_rfc3339(), "2024-01-02T00:00:00+00:00");
        assert_eq!(d.to.unwrap().to_rfc3339(), "2024-01-03T00:00:00+00:00");
        let d = DateRange::parse(">2024-01-02").unwrap();
        assert_eq!(d.from.unwrap().to_rfc3339(), "2024-01-03T00:00:00+00:00");
        let d = DateRange::parse("<=2024-01-02").unwrap();
        assert_eq!(d.to.unwrap().to_rfc3339(), "2024-01-03T00:00:00+00:00");
        let d = DateRange::parse("2024-01-01..2024-01-31").unwrap();
        assert_eq!(d.to.unwrap().to_rfc3339(), "2024-02-01T00:00:00+00:00");
        let d = DateRange::parse("*..2024-01-31").unwrap();
        assert!(d.from.is_none());
        let d = DateRange::parse(">=2024-01-01T10:00:00Z").unwrap();
        assert_eq!(d.from.unwrap().to_rfc3339(), "2024-01-01T10:00:00+00:00");
        let d = DateRange::parse("2024-01-01T10:00:00+02:00").unwrap();
        assert_eq!(d.from.unwrap().to_rfc3339(), "2024-01-01T08:00:00+00:00");
        assert!(DateRange::parse("yesterday").is_none());
    }

    #[test]
    fn helpers() {
        assert_eq!(like_escape("50%_a\\"), "50\\%\\_a\\\\");
        assert_eq!(
            tsquery_words("Fix: crash-on & 'quote'"),
            vec!["fix", "crash", "on", "quote"]
        );
        assert_eq!(comma_list("a, b,,c"), vec!["a", "b", "c"]);
    }
}
