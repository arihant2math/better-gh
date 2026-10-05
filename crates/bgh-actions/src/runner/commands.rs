//! Workflow commands (`::name k=v::data` on stdout) and file commands
//! (`GITHUB_OUTPUT`, `GITHUB_ENV`, ... contents).

use indexmap::IndexMap;

/// A parsed `::command` line.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowCommand {
    pub name: String,
    pub props: IndexMap<String, String>,
    pub data: String,
}

/// Unescape command data (`%25`, `%0D`, `%0A`).
pub fn unescape_data(s: &str) -> String {
    s.replace("%0D", "\r")
        .replace("%0A", "\n")
        .replace("%25", "%")
}

/// Unescape a command property (`%25`, `%0D`, `%0A`, `%3A`, `%2C`).
pub fn unescape_property(s: &str) -> String {
    s.replace("%0D", "\r")
        .replace("%0A", "\n")
        .replace("%3A", ":")
        .replace("%2C", ",")
        .replace("%25", "%")
}

/// Parse a `::name prop=value,...::data` line. Returns `None` for ordinary
/// output.
pub fn parse_command(line: &str) -> Option<WorkflowCommand> {
    let line = line.trim_start();
    let rest = line.strip_prefix("::")?;
    let end = rest.find("::")?;
    let header = &rest[..end];
    let data = &rest[end + 2..];
    let (name, props_src) = match header.find(' ') {
        Some(i) => (&header[..i], header[i + 1..].trim()),
        None => (header, ""),
    };
    if name.is_empty() {
        return None;
    }
    let mut props = IndexMap::new();
    for pair in props_src.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once('=') {
            props.insert(k.trim().to_string(), unescape_property(v));
        }
    }
    Some(WorkflowCommand {
        name: name.to_string(),
        props,
        data: unescape_data(data),
    })
}

/// Parse a `GITHUB_OUTPUT` / `GITHUB_ENV` / `GITHUB_STATE` file: `name=value`
/// lines and `name<<DELIMITER` heredocs.
pub fn parse_file_commands(content: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut lines = content
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l));
    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        let eq = line.find('=');
        let heredoc = line.find("<<");
        match (eq, heredoc) {
            (eq, Some(h)) if eq.is_none_or(|e| h < e) => {
                let name = line[..h].trim().to_string();
                let delim = line[h + 2..].trim().to_string();
                if name.is_empty() || delim.is_empty() {
                    return Err(format!("Invalid format '{line}'"));
                }
                let mut value: Vec<&str> = Vec::new();
                let mut closed = false;
                for l in lines.by_ref() {
                    if l == delim {
                        closed = true;
                        break;
                    }
                    value.push(l);
                }
                if !closed {
                    return Err(format!(
                        "Matching delimiter not found '{delim}' for '{name}'"
                    ));
                }
                out.push((name, value.join("\n")));
            }
            (Some(e), _) => {
                let name = line[..e].to_string();
                if name.is_empty() {
                    return Err(format!("Invalid format '{line}'"));
                }
                out.push((name, line[e + 1..].to_string()));
            }
            _ => return Err(format!("Invalid format '{line}'")),
        }
    }
    Ok(out)
}

/// Split a command line into words (POSIX-ish: single quotes, double quotes
/// with backslash escapes, backslash escapes outside quotes).
pub fn split_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    cur.push(c);
                }
            }
            '"' => {
                in_word = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.peek() {
                            Some(&n) if matches!(n, '"' | '\\' | '$' | '`') => {
                                cur.push(n);
                                chars.next();
                            }
                            _ => cur.push('\\'),
                        },
                        c => cur.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    words
}

/// Quote a word for display / `sh -c`.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@+%".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        let c = parse_command("::error file=a.rs,line=3,title=T%3A x::boom%0Anext").unwrap();
        assert_eq!(c.name, "error");
        assert_eq!(c.props["file"], "a.rs");
        assert_eq!(c.props["line"], "3");
        assert_eq!(c.props["title"], "T: x");
        assert_eq!(c.data, "boom\nnext");
        let c = parse_command("::add-mask::secret").unwrap();
        assert_eq!(c.name, "add-mask");
        assert_eq!(c.data, "secret");
        let c = parse_command("::endgroup::").unwrap();
        assert_eq!(c.name, "endgroup");
        assert!(parse_command("hello ::x::").is_none());
        assert!(parse_command("::nope").is_none());
        assert_eq!(unescape_data("100%25%0D%0A"), "100%\r\n");
    }

    #[test]
    fn parses_file_commands() {
        let v = parse_file_commands("a=1\nb<<EOF\nx\ny=z\nEOF\n\nc=d=e\n").unwrap();
        assert_eq!(
            v,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "x\ny=z".into()),
                ("c".into(), "d=e".into())
            ]
        );
        assert!(parse_file_commands("x<<E\nno end").is_err());
        assert!(parse_file_commands("garbage").is_err());
    }

    #[test]
    fn splits_words() {
        assert_eq!(
            split_words(r#"a 'b c' "d \"e\"" f\ g"#),
            vec!["a", "b c", "d \"e\"", "f g"]
        );
        assert!(split_words("   ").is_empty());
        assert_eq!(split_words("x ''"), vec!["x", ""]);
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("abc"), "abc");
    }
}
