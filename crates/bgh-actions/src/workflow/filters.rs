//! Branch / tag / path filter patterns ("Filter pattern cheat sheet").
//!
//! * `*`  zero or more characters, but not `/`
//! * `**` zero or more of any character (a `**/` prefix may also match no
//!   directory at all, so `**/docs/**` matches `docs/a.md`)
//! * `?`  zero or one of the preceding character
//! * `+`  one or more of the preceding character
//! * `[]` one character from the class; ranges like `[0-9a-z]`
//! * `!`  at the start of a pattern negates it (see [`filter_matches`])
//! * `\`  escapes the next character
//!
//! Patterns are anchored: they must match the whole string.

use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
enum Atom {
    Char(char),
    Class(Vec<(char, char)>),
    /// `*`
    Star,
    /// `**`
    DoubleStar,
    /// `**/`: empty, or anything ending with `/`
    DoubleStarSlash,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Quant {
    One,
    ZeroOrOne,
    OneOrMore,
}

#[derive(Debug, Clone, PartialEq)]
struct Piece {
    atom: Atom,
    quant: Quant,
}

/// A compiled filter pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pieces: Vec<Piece>,
}

impl Pattern {
    /// Compiles a pattern (without interpreting a leading `!`).
    pub fn compile(pattern: &str) -> Pattern {
        let chars: Vec<char> = pattern.chars().collect();
        let mut pieces: Vec<Piece> = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            match c {
                '\\' if i + 1 < chars.len() => {
                    pieces.push(Piece {
                        atom: Atom::Char(chars[i + 1]),
                        quant: Quant::One,
                    });
                    i += 2;
                }
                '*' => {
                    if chars.get(i + 1) == Some(&'*') {
                        let mut j = i + 2;
                        while chars.get(j) == Some(&'*') {
                            j += 1;
                        }
                        if chars.get(j) == Some(&'/') {
                            pieces.push(Piece {
                                atom: Atom::DoubleStarSlash,
                                quant: Quant::One,
                            });
                            i = j + 1;
                        } else {
                            pieces.push(Piece {
                                atom: Atom::DoubleStar,
                                quant: Quant::One,
                            });
                            i = j;
                        }
                    } else {
                        pieces.push(Piece {
                            atom: Atom::Star,
                            quant: Quant::One,
                        });
                        i += 1;
                    }
                }
                '?' | '+' => {
                    let quant = if c == '?' {
                        Quant::ZeroOrOne
                    } else {
                        Quant::OneOrMore
                    };
                    match pieces.last_mut() {
                        Some(p)
                            if p.quant == Quant::One
                                && matches!(p.atom, Atom::Char(_) | Atom::Class(_)) =>
                        {
                            p.quant = quant;
                        }
                        // Quantifier after a wildcard is redundant.
                        Some(p) if matches!(p.atom, Atom::Star | Atom::DoubleStar) => {}
                        // Nothing to quantify: treat literally.
                        _ => pieces.push(Piece {
                            atom: Atom::Char(c),
                            quant: Quant::One,
                        }),
                    }
                    i += 1;
                }
                '[' => match parse_class(&chars, i) {
                    Some((ranges, next)) => {
                        pieces.push(Piece {
                            atom: Atom::Class(ranges),
                            quant: Quant::One,
                        });
                        i = next;
                    }
                    None => {
                        pieces.push(Piece {
                            atom: Atom::Char('['),
                            quant: Quant::One,
                        });
                        i += 1;
                    }
                },
                _ => {
                    pieces.push(Piece {
                        atom: Atom::Char(c),
                        quant: Quant::One,
                    });
                    i += 1;
                }
            }
        }
        Pattern { pieces }
    }

    /// Whether the pattern matches the whole of `s`.
    pub fn is_match(&self, s: &str) -> bool {
        let chars: Vec<char> = s.chars().collect();
        let mut failed = HashSet::new();
        self.match_at(0, 0, &chars, &mut failed)
    }

    fn match_at(
        &self,
        pi: usize,
        si: usize,
        s: &[char],
        failed: &mut HashSet<(usize, usize)>,
    ) -> bool {
        if pi == self.pieces.len() {
            return si == s.len();
        }
        if failed.contains(&(pi, si)) {
            return false;
        }
        let piece = &self.pieces[pi];
        let ok = match &piece.atom {
            Atom::Star => {
                let end = s[si..]
                    .iter()
                    .position(|c| *c == '/')
                    .map_or(s.len(), |p| si + p);
                (si..=end).any(|k| self.match_at(pi + 1, k, s, failed))
            }
            Atom::DoubleStar => (si..=s.len()).any(|k| self.match_at(pi + 1, k, s, failed)),
            Atom::DoubleStarSlash => {
                self.match_at(pi + 1, si, s, failed)
                    || (si..s.len()).any(|k| s[k] == '/' && self.match_at(pi + 1, k + 1, s, failed))
            }
            atom => {
                let one = |k: usize| k < s.len() && atom_matches(atom, s[k]);
                match piece.quant {
                    Quant::One => one(si) && self.match_at(pi + 1, si + 1, s, failed),
                    Quant::ZeroOrOne => {
                        self.match_at(pi + 1, si, s, failed)
                            || (one(si) && self.match_at(pi + 1, si + 1, s, failed))
                    }
                    Quant::OneOrMore => {
                        let mut k = si;
                        let mut found = false;
                        while one(k) {
                            k += 1;
                            if self.match_at(pi + 1, k, s, failed) {
                                found = true;
                                break;
                            }
                        }
                        found
                    }
                }
            }
        };
        if !ok {
            failed.insert((pi, si));
        }
        ok
    }
}

fn atom_matches(atom: &Atom, c: char) -> bool {
    match atom {
        Atom::Char(x) => *x == c,
        Atom::Class(ranges) => ranges.iter().any(|(lo, hi)| *lo <= c && c <= *hi),
        _ => false,
    }
}

/// Parses `[...]` starting at `start` (which holds `[`). Returns the ranges
/// and the index after `]`, or `None` if the class is unterminated/empty.
fn parse_class(chars: &[char], start: usize) -> Option<(Vec<(char, char)>, usize)> {
    let mut ranges = Vec::new();
    let mut i = start + 1;
    while i < chars.len() {
        let mut c = chars[i];
        if c == ']' {
            return if ranges.is_empty() {
                None
            } else {
                Some((ranges, i + 1))
            };
        }
        if c == '\\' && i + 1 < chars.len() {
            i += 1;
            c = chars[i];
        }
        if chars.get(i + 1) == Some(&'-')
            && let Some(&hi) = chars.get(i + 2)
            && hi != ']'
        {
            let (lo, hi) = if c <= hi { (c, hi) } else { (hi, c) };
            ranges.push((lo, hi));
            i += 3;
            continue;
        }
        ranges.push((c, c));
        i += 1;
    }
    None
}

/// Matches a single pattern against `s` (a leading `!` is NOT interpreted).
pub fn glob_match(pattern: &str, s: &str) -> bool {
    Pattern::compile(pattern).is_match(s)
}

/// Evaluates a filter list: patterns are evaluated in order and the last
/// matching pattern wins; a pattern starting with `!` is negative. Returns
/// true when the last matching pattern is positive (false if none match).
pub fn filter_matches(patterns: &[String], s: &str) -> bool {
    let mut result = false;
    for p in patterns {
        let (negated, body) = match p.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, p.as_str()),
        };
        if glob_match(body, s) {
            result = !negated;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pats(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn star_does_not_cross_slash() {
        assert!(glob_match("*", "main"));
        assert!(glob_match("*", "releases"));
        assert!(!glob_match("*", "feature/x"));
        assert!(glob_match("*", ""));
    }

    #[test]
    fn double_star_matches_everything() {
        assert!(glob_match("**", "a/b/c"));
        assert!(glob_match("**", "main"));
        assert!(glob_match("**", ""));
    }

    #[test]
    fn feature_star_vs_double_star() {
        assert!(glob_match("feature/*", "feature/my-branch"));
        assert!(glob_match("feature/*", "feature/your-branch"));
        assert!(!glob_match("feature/*", "feature/a/b"));
        assert!(glob_match("feature/**", "feature/beta-a/my-branch"));
        assert!(glob_match("feature/**", "feature/mona/the/octocat"));
        assert!(glob_match("feature/**", "feature/x"));
        assert!(!glob_match("feature/**", "features/x"));
    }

    #[test]
    fn main_and_release_alternatives() {
        let p = pats(&["main", "releases/mona-the-octocat"]);
        assert!(filter_matches(&p, "main"));
        assert!(filter_matches(&p, "releases/mona-the-octocat"));
        assert!(!filter_matches(&p, "releases/other"));
    }

    #[test]
    fn js_extensions() {
        assert!(glob_match("*.js", "app.js"));
        assert!(glob_match("*.js", "index.js"));
        assert!(!glob_match("*.js", "js/index.js"));
        assert!(glob_match("**.js", "index.js"));
        assert!(glob_match("**.js", "js/index.js"));
        assert!(glob_match("**.js", "src/js/app.js"));
        assert!(!glob_match("**.js", "src/js/app.jsx"));
    }

    #[test]
    fn docs_directories() {
        assert!(glob_match("docs/*", "docs/README.md"));
        assert!(!glob_match("docs/*", "docs/a/b.md"));
        assert!(glob_match("docs/**", "docs/README.md"));
        assert!(glob_match("docs/**", "docs/mona/octocat.txt"));
        assert!(!glob_match("docs/**", "src/docs/x.md"));
        assert!(glob_match("docs/**/*.md", "docs/a/b/c.md"));
        assert!(glob_match("docs/**/*.md", "docs/c.md"));
        assert!(!glob_match("docs/**/*.md", "docs/a/c.txt"));
    }

    #[test]
    fn double_star_docs_anywhere() {
        assert!(glob_match("**/docs/**", "/docs/hello.md"));
        assert!(glob_match("**/docs/**", "dir/docs/my-file.txt"));
        assert!(glob_match("**/docs/**", "space/docs/plan/space.doc"));
        assert!(glob_match("**/docs/**", "docs/top.md"));
        assert!(!glob_match("**/docs/**", "mydocs/top.md"));
        assert!(glob_match("**/README.md", "README.md"));
        assert!(glob_match("**/README.md", "server/README.md"));
        assert!(glob_match("**/*src/**", "a/src/app.js"));
        assert!(glob_match("**/*src/**", "my-src/code/js/app.js"));
        assert!(glob_match("**/*-post.md", "my-post.md"));
        assert!(glob_match("**/*-post.md", "path/their-post.md"));
        assert!(glob_match("**/migrate-*.sql", "migrate-10909.sql"));
        assert!(glob_match("**/migrate-*.sql", "db/migrate-v1.0.sql"));
        assert!(glob_match("**/migrate-*.sql", "db/sept/migrate-v1.sql"));
    }

    #[test]
    fn question_mark_is_optional_previous_char() {
        assert!(glob_match("*.jsx?", "page.js"));
        assert!(glob_match("*.jsx?", "page.jsx"));
        assert!(!glob_match("*.jsx?", "page.jsxx"));
        assert!(glob_match("colou?r", "color"));
        assert!(glob_match("colou?r", "colour"));
    }

    #[test]
    fn plus_is_one_or_more_previous_char() {
        assert!(glob_match("v2+", "v2"));
        assert!(glob_match("v2+", "v222"));
        assert!(!glob_match("v2+", "v"));
        assert!(!glob_match("v2+", "v23"));
    }

    #[test]
    fn version_tag_pattern() {
        let p = "v[12].[0-9]+.[0-9]+";
        assert!(glob_match(p, "v1.10.1"));
        assert!(glob_match(p, "v2.0.0"));
        assert!(!glob_match(p, "v3.0.0"));
        assert!(!glob_match(p, "v1.x.0"));
        assert!(!glob_match(p, "v1.0"));
        assert!(!glob_match(p, "v1.0.0-beta"));
    }

    #[test]
    fn char_class_ranges() {
        assert!(glob_match("[0-9a-z]", "q"));
        assert!(glob_match("[0-9a-z]", "7"));
        assert!(!glob_match("[0-9a-z]", "Q"));
        assert!(glob_match("[CB]at", "Cat"));
        assert!(glob_match("[CB]at", "Bat"));
        assert!(!glob_match("[CB]at", "Rat"));
        assert!(glob_match("[1-2]00", "100"));
        assert!(glob_match("[1-2]00", "200"));
        assert!(!glob_match("[1-2]00", "300"));
    }

    #[test]
    fn escapes() {
        assert!(glob_match("a\\*b", "a*b"));
        assert!(!glob_match("a\\*b", "axb"));
        assert!(glob_match("foo\\+", "foo+"));
        assert!(glob_match("\\!important", "!important"));
        assert!(glob_match("\\[x]", "[x]"));
    }

    #[test]
    fn literal_special_chars_without_meaning() {
        // unterminated class and leading quantifier are literal
        assert!(glob_match("a[b", "a[b"));
        assert!(glob_match("+x", "+x"));
        assert!(glob_match("releases/**-alpha", "releases/beta/3-alpha"));
    }

    #[test]
    fn negation_last_match_wins() {
        let p = pats(&["releases/**", "!releases/**-alpha"]);
        assert!(filter_matches(&p, "releases/10"));
        assert!(filter_matches(&p, "releases/beta/mona"));
        assert!(!filter_matches(&p, "releases/10-alpha"));
        assert!(!filter_matches(&p, "releases/beta/3-alpha"));
        assert!(!filter_matches(&p, "main"));

        let p = pats(&["sub-project/**", "!sub-project/docs/**"]);
        assert!(filter_matches(&p, "sub-project/src/a.rs"));
        assert!(!filter_matches(&p, "sub-project/docs/a.md"));

        let p = pats(&["!a", "*"]);
        assert!(
            filter_matches(&p, "a"),
            "later positive pattern re-includes"
        );
    }

    #[test]
    fn empty_list_matches_nothing() {
        assert!(!filter_matches(&[], "main"));
    }

    #[test]
    fn no_catastrophic_backtracking() {
        let s = "a".repeat(60) + "b";
        assert!(!glob_match("*a*a*a*a*a*a*a*a*a*a*c", &s));
        assert!(!glob_match("**a**a**a**a**a**a**c", &s));
    }
}
