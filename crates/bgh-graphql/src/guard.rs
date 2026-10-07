//! Pre-parse guard on the raw query text (issue #335).
//!
//! async-graphql's pest parser recurses once (several frames) per nesting
//! level of `{`, `(` and `[`, so a ~40 KB query nested a few thousand levels
//! deep overflows the worker's stack and aborts the process before any of
//! the schema's limits (`limit_depth`, the node limit) run. This scan bounds
//! the nesting and the length in one allocation-free pass, skipping string
//! literals (`"..."` with escapes, `"""block strings"""`) and `#` comments
//! the way the GraphQL lexer does, so brackets inside them don't count.

/// Longest accepted query, in bytes. `gh`'s largest queries are a few KB;
/// a 100-alias query is ~25 KB.
pub const MAX_QUERY_LENGTH: usize = 256 * 1024;

/// Deepest accepted nesting of `{`, `(` and `[` combined. Selection depth
/// is capped at 32 after parsing (`limit_depth`), so legitimate queries
/// stay far below this; the parser alone survives over 20x this depth on a
/// 2 MiB stack in a debug build.
pub const MAX_NESTING: usize = 128;

/// Why a query was rejected before parsing.
#[derive(Debug, PartialEq, Eq)]
pub enum Rejection {
    TooLong(usize),
    TooDeep,
}

impl Rejection {
    /// GitHub-style error class and message.
    pub fn error(&self) -> (&'static str, String) {
        match self {
            Rejection::TooLong(len) => (
                "MAX_QUERY_LENGTH_EXCEEDED",
                format!(
                    "Query is {len} bytes long, which exceeds the maximum of {MAX_QUERY_LENGTH} bytes."
                ),
            ),
            Rejection::TooDeep => (
                "MAX_NESTING_EXCEEDED",
                format!("Query nesting exceeds the maximum depth of {MAX_NESTING}."),
            ),
        }
    }
}

/// Check `query` before it reaches the parser.
pub fn check(query: &str) -> Result<(), Rejection> {
    if query.len() > MAX_QUERY_LENGTH {
        return Err(Rejection::TooLong(query.len()));
    }
    let b = query.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' | b'(' | b'[' => {
                depth += 1;
                if depth > MAX_NESTING {
                    return Err(Rejection::TooDeep);
                }
            }
            b'}' | b')' | b']' => depth = depth.saturating_sub(1),
            b'#' => {
                while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                    i += 1;
                }
                continue;
            }
            b'"' if b[i..].starts_with(b"\"\"\"") => {
                // Block string: only `\"""` escapes; ends at the next `"""`.
                i += 3;
                while i < b.len() && !b[i..].starts_with(b"\"\"\"") {
                    i += if b[i..].starts_with(b"\\\"\"\"") {
                        4
                    } else {
                        1
                    };
                }
                i += 3;
                continue;
            }
            b'"' => {
                // Plain string: `\` escapes the next byte; a line break is
                // a lexer error, so stop skipping there (counting what
                // follows is the conservative choice).
                i += 1;
                while i < b.len() && !matches!(b[i], b'"' | b'\n' | b'\r') {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                if i < b.len() && b[i] == b'"' {
                    i += 1;
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested(open: &str, close: &str, n: usize) -> String {
        format!("{}{}", open.repeat(n), close.repeat(n))
    }

    #[test]
    fn bounds_nesting() {
        assert_eq!(check(&nested("{", "}", MAX_NESTING)), Ok(()));
        assert_eq!(
            check(&nested("{", "}", MAX_NESTING + 1)),
            Err(Rejection::TooDeep)
        );
        assert_eq!(check(&nested("([", "])", 65)), Err(Rejection::TooDeep));
        // Siblings don't accumulate.
        assert_eq!(check(&"{a}".repeat(10_000)), Ok(()));
    }

    #[test]
    fn skips_strings_and_comments() {
        let deep = "{".repeat(10_000);
        assert_eq!(check(&format!(r#"{{ a(x: "{deep}") }}"#)), Ok(()));
        assert_eq!(check(&format!(r#"{{ a(x: "\"{deep}\\") }}"#)), Ok(()));
        assert_eq!(
            check(&format!(r#"{{ a(x: """{deep} \""" "{deep}""") }}"#)),
            Ok(())
        );
        assert_eq!(check(&format!("# {deep}\n{{ a }}")), Ok(()));
        // A string doesn't extend past a line break, nor a comment past one.
        assert_eq!(
            check(&format!("{{ a(x: \"\n{deep}")),
            Err(Rejection::TooDeep)
        );
        assert_eq!(check(&format!("# x\r{deep}")), Err(Rejection::TooDeep));
        // Unterminated strings are fine (the parser reports them).
        assert_eq!(check(r#"{ a(x: """unterminated"#), Ok(()));
        assert_eq!(check(r#"{ a(x: "trailing\"#), Ok(()));
    }

    #[test]
    fn bounds_length() {
        let q = format!("{{ a }}{}", " ".repeat(MAX_QUERY_LENGTH));
        assert_eq!(check(&q), Err(Rejection::TooLong(q.len())));
    }
}
