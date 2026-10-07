//! Tokenizer for `${{ }}` expressions, plus the template scanner used to find
//! the closing `}}` of an embedded expression.

use super::ExprError;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    Number(f64),
    Str(String),
    /// Identifier or keyword (`true`, `false`, `null`, `NaN`, `Infinity`).
    /// Keywords are resolved by the parser so they still work as property
    /// names after a `.`.
    Ident(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    Comma,
    Star,
    Not,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub tok: Tok,
    /// Byte offset of the token in the source.
    pub pos: usize,
    /// Source text of the token (for error messages).
    pub text: String,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

pub(crate) fn tokenize(src: &str) -> Result<Vec<Token>, ExprError> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0;
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let byte = |i: usize| chars.get(i).map(|&(b, _)| b).unwrap_or(src.len());

    while i < chars.len() {
        let (pos, c) = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Whether the previous token ends a value; decides whether `.5` is a
        // number literal or a dereference.
        let after_value = matches!(
            out.last().map(|t| &t.tok),
            Some(
                Tok::Ident(_)
                    | Tok::Number(_)
                    | Tok::Str(_)
                    | Tok::RParen
                    | Tok::RBracket
                    | Tok::Star
            )
        );
        let next = at(i + 1);
        let simple = |tok: Tok, len: usize| Token {
            tok,
            pos,
            text: src[pos..byte(i + len)].to_string(),
        };
        let (token, len) = match c {
            '(' => (simple(Tok::LParen, 1), 1),
            ')' => (simple(Tok::RParen, 1), 1),
            '[' => (simple(Tok::LBracket, 1), 1),
            ']' => (simple(Tok::RBracket, 1), 1),
            ',' => (simple(Tok::Comma, 1), 1),
            '*' => (simple(Tok::Star, 1), 1),
            '!' if next == Some('=') => (simple(Tok::Ne, 2), 2),
            '!' => (simple(Tok::Not, 1), 1),
            '<' if next == Some('=') => (simple(Tok::Le, 2), 2),
            '<' => (simple(Tok::Lt, 1), 1),
            '>' if next == Some('=') => (simple(Tok::Ge, 2), 2),
            '>' => (simple(Tok::Gt, 1), 1),
            '=' if next == Some('=') => (simple(Tok::Eq, 2), 2),
            '&' if next == Some('&') => (simple(Tok::And, 2), 2),
            '|' if next == Some('|') => (simple(Tok::Or, 2), 2),
            '.' if after_value || !next.is_some_and(|n| n.is_ascii_digit()) => {
                (simple(Tok::Dot, 1), 1)
            }
            '\'' => {
                let mut s = String::new();
                let mut j = i + 1;
                loop {
                    match at(j) {
                        None => {
                            return Err(ExprError::UnterminatedString {
                                pos,
                                expr: src.to_string(),
                            });
                        }
                        Some('\'') if at(j + 1) == Some('\'') => {
                            s.push('\'');
                            j += 2;
                        }
                        Some('\'') => {
                            j += 1;
                            break;
                        }
                        Some(ch) => {
                            s.push(ch);
                            j += 1;
                        }
                    }
                }
                (
                    Token {
                        tok: Tok::Str(s),
                        pos,
                        text: src[pos..byte(j)].to_string(),
                    },
                    j - i,
                )
            }
            c if c.is_ascii_digit()
                || c == '.'
                || (c == '-'
                    && (next.is_some_and(|n| n.is_ascii_digit() || n == '.')
                        || src[pos..].starts_with("-Infinity"))) =>
            {
                lex_number(src, &chars, i)?
            }
            c if is_ident_start(c) => {
                let mut j = i;
                while at(j).is_some_and(is_ident_char) {
                    j += 1;
                }
                let text = src[pos..byte(j)].to_string();
                (
                    Token {
                        tok: Tok::Ident(text.clone()),
                        pos,
                        text,
                    },
                    j - i,
                )
            }
            other => {
                return Err(ExprError::UnexpectedChar {
                    ch: other,
                    pos,
                    expr: src.to_string(),
                });
            }
        };
        out.push(token);
        i += len;
    }
    Ok(out)
}

fn lex_number(
    src: &str,
    chars: &[(usize, char)],
    start: usize,
) -> Result<(Token, usize), ExprError> {
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let byte = |i: usize| chars.get(i).map(|&(b, _)| b).unwrap_or(src.len());
    let pos = chars[start].0;
    let mut j = start;
    let negative = at(j) == Some('-');
    if negative {
        j += 1;
    }
    if src[byte(j)..].starts_with("Infinity") {
        j += "Infinity".len();
    } else if at(j) == Some('0') && matches!(at(j + 1), Some('x' | 'X')) {
        j += 2;
        while at(j).is_some_and(|c| c.is_ascii_hexdigit()) {
            j += 1;
        }
    } else {
        while at(j).is_some_and(|c| c.is_ascii_digit() || c == '.') {
            j += 1;
        }
        if matches!(at(j), Some('e' | 'E')) {
            j += 1;
            if matches!(at(j), Some('+' | '-')) {
                j += 1;
            }
            while at(j).is_some_and(|c| c.is_ascii_digit()) {
                j += 1;
            }
        }
    }
    // A number must not run straight into identifier characters (`12abc`).
    let mut end = j;
    while at(end).is_some_and(|c| is_ident_char(c) || c == '.') {
        end += 1;
    }
    let text = src[pos..byte(end)].to_string();
    let invalid = || ExprError::InvalidNumber {
        token: text.clone(),
        pos,
        expr: src.to_string(),
    };
    if end != j {
        return Err(invalid());
    }
    let body = if negative { &text[1..] } else { &text[..] };
    let magnitude = if body == "Infinity" {
        f64::INFINITY
    } else if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|_| invalid())? as f64
    } else {
        parse_decimal(body).ok_or_else(invalid)?
    };
    let value = if negative { -magnitude } else { magnitude };
    Ok((
        Token {
            tok: Tok::Number(value),
            pos,
            text: text.clone(),
        },
        end - start,
    ))
}

/// Strict decimal parse: digits, optional fraction, optional exponent. Rejects
/// Rust-only spellings such as `inf` / `nan`.
pub(crate) fn parse_decimal(s: &str) -> Option<f64> {
    let ok = !s.is_empty()
        && s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
    if !ok {
        return None;
    }
    s.parse::<f64>().ok()
}

/// Given `template` and the byte index just after a `${{`, return the byte
/// index of the matching `}}`. Single-quoted string literals (with `''`
/// escapes) are skipped, so `}}` inside a string does not close the
/// expression.
pub(crate) fn find_closing(template: &str, from: usize) -> Result<usize, ExprError> {
    let bytes = template.as_bytes();
    let mut i = from;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if b == b'\'' {
                if bytes.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                in_string = false;
            }
        } else if b == b'\'' {
            in_string = true;
        } else if b == b'}' && bytes.get(i + 1) == Some(&b'}') {
            return Ok(i);
        }
        i += 1;
    }
    Err(ExprError::UnclosedExpression {
        pos: from.saturating_sub(3),
        template: template.to_string(),
    })
}
