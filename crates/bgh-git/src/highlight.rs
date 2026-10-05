//! Server-side syntax highlighting (syntect grammars + two-face's extra
//! syntaxes) into per-line, class-based HTML.
//!
//! Output uses CSS classes (`hl-keyword`, `hl-string`, ...) rather than
//! inline colors, so one cached rendering serves both light and dark themes
//! (see [`css`]). Every line is self-contained: spans still open at the end
//! of a line are closed and re-opened on the next one, so clients can
//! render (and virtualize) lines independently.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use syntect::highlighting::ThemeSet;
use syntect::html::{ClassStyle, css_for_theme_with_class_style, line_tokens_to_classed_spans};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

/// Class prefix of every emitted span.
pub const CLASS_PREFIX: &str = "hl-";
const STYLE: ClassStyle = ClassStyle::SpacedPrefixed { prefix: "hl-" };

/// Files larger than this are returned as escaped plain text.
pub const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
/// Files with more lines than this are returned as escaped plain text.
pub const MAX_HIGHLIGHT_LINES: usize = 20_000;
/// Time budget per file; remaining lines are emitted unhighlighted.
const TIME_BUDGET: Duration = Duration::from_secs(2);

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

/// Load the grammars now (they are otherwise loaded on first use).
pub fn warm_up() {
    LazyLock::force(&SYNTAXES);
}

/// Highlighting result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Highlighted {
    /// Grammar name (`Rust`, `TypeScript`, ...); `None` for plain text.
    pub language: Option<String>,
    /// One HTML fragment per line (no trailing newline).
    pub lines: Vec<String>,
    /// False when the file was too large / slow and is plain escaped text.
    pub highlighted: bool,
}

/// Pick a grammar from the file name, then the first line (shebang, modeline).
pub fn find_syntax(path: &str, text: &str) -> Option<&'static SyntaxReference> {
    let ss = &*SYNTAXES;
    let name = path.rsplit('/').next().unwrap_or(path);
    let by_name = match name {
        "Dockerfile" | "Containerfile" => ss.find_syntax_by_name("Dockerfile"),
        "Makefile" | "GNUmakefile" | "makefile" => ss.find_syntax_by_name("Makefile"),
        "CMakeLists.txt" => ss.find_syntax_by_name("CMake"),
        "Cargo.lock" | "Pipfile" | "poetry.lock" => ss.find_syntax_by_extension("toml"),
        "go.mod" | "go.sum" => None,
        _ => None,
    };
    by_name
        .or_else(|| ss.find_syntax_by_extension(name))
        .or_else(|| {
            name.rsplit_once('.')
                .and_then(|(_, ext)| ss.find_syntax_by_extension(ext))
        })
        .or_else(|| {
            let first = text.lines().next().unwrap_or("");
            ss.find_syntax_by_first_line(first)
        })
        .filter(|s| s.name != "Plain Text")
}

/// HTML-escape for text nodes.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Split into lines (`\n` / `\r\n`), dropping a final empty line.
pub fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
}

/// Escaped, unhighlighted lines.
pub fn plain(text: &str) -> Vec<String> {
    split_lines(text).map(escape).collect()
}

fn open_spans(out: &mut String, stack: &ScopeStack) {
    for scope in stack.as_slice() {
        out.push_str("<span class=\"");
        for (i, atom) in scope.build_string().split('.').enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(CLASS_PREFIX);
            out.push_str(atom);
        }
        out.push_str("\">");
    }
}

/// Highlight `text` (the contents of `path`).
pub fn highlight(path: &str, text: &str) -> Highlighted {
    let syntax = find_syntax(path, text);
    let language = syntax.map(|s| s.name.clone());
    let Some(syntax) = syntax else {
        return Highlighted {
            language,
            lines: plain(text),
            highlighted: false,
        };
    };
    if text.len() > MAX_HIGHLIGHT_BYTES || text.lines().count() > MAX_HIGHLIGHT_LINES {
        return Highlighted {
            language,
            lines: plain(text),
            highlighted: false,
        };
    }
    let started = Instant::now();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut lines = Vec::new();
    let mut highlighted = true;
    for line in LinesWithEndings::from(text) {
        if highlighted && started.elapsed() > TIME_BUDGET {
            highlighted = false;
        }
        let content = line.trim_end_matches(['\n', '\r']);
        if !highlighted {
            lines.push(escape(content));
            continue;
        }
        let ops = match state.parse_line(line, &SYNTAXES) {
            Ok(ops) => ops,
            Err(_) => {
                highlighted = false;
                lines.push(escape(content));
                continue;
            }
        };
        let mut html = String::with_capacity(line.len() * 2);
        let reopened = stack.len() as isize;
        open_spans(&mut html, &stack);
        match line_tokens_to_classed_spans(line, &ops, STYLE, &mut stack) {
            Ok((body, delta)) => {
                html.push_str(body.trim_end_matches(['\n', '\r']));
                // Newlines are only ever at the end of the text, but may
                // precede closing tags.
                if html.contains(['\n', '\r']) {
                    html.retain(|c| c != '\n' && c != '\r');
                }
                for _ in 0..(reopened + delta).max(0) {
                    html.push_str("</span>");
                }
                lines.push(html);
            }
            Err(_) => {
                highlighted = false;
                lines.push(escape(content));
            }
        }
    }
    Highlighted {
        language,
        lines,
        highlighted,
    }
}

/// Stylesheet for the `hl-` classes: a light theme by default and a dark
/// theme under `prefers-color-scheme: dark` / `[data-theme="dark"]`.
pub fn css() -> String {
    let themes = ThemeSet::load_defaults();
    let light =
        css_for_theme_with_class_style(&themes.themes["InspiredGitHub"], STYLE).unwrap_or_default();
    let dark = css_for_theme_with_class_style(&themes.themes["base16-ocean.dark"], STYLE)
        .unwrap_or_default();
    let scoped_dark = scope_css(&dark, "[data-theme=\"dark\"] ");
    format!(
        "{light}\n@media (prefers-color-scheme: dark) {{\n{}\n}}\n{scoped_dark}\n",
        scope_css(&dark, ":root:not([data-theme=\"light\"]) ")
    )
}

/// Prefix every selector of a generated stylesheet.
fn scope_css(css: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(css.len() * 2);
    for line in css.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('.') {
            let scoped = trimmed
                .split(", ")
                .map(|sel| format!("{prefix}{sel}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&scoped);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_per_line() {
        let src = "/* multi\nline */\nfn main() {\n    let s = \"a<b\";\n}\n";
        let h = highlight("src/main.rs", src);
        assert_eq!(h.language.as_deref(), Some("Rust"));
        assert!(h.highlighted);
        assert_eq!(h.lines.len(), 5);
        // Each line is balanced.
        for l in &h.lines {
            assert_eq!(
                l.matches("<span").count(),
                l.matches("</span>").count(),
                "{l}"
            );
            assert!(!l.contains('\n'));
        }
        assert!(h.lines[1].contains("hl-comment"), "{}", h.lines[1]);
        assert!(h.lines[3].contains("a&lt;b"));
        assert!(h.lines[2].contains("hl-"));
    }

    #[test]
    fn detects_languages() {
        assert_eq!(
            find_syntax("web/app.tsx", "").map(|s| s.name.as_str()),
            Some("TypeScriptReact")
        );
        assert_eq!(
            find_syntax("Dockerfile", "").map(|s| s.name.as_str()),
            Some("Dockerfile")
        );
        assert_eq!(
            find_syntax("script", "#!/usr/bin/env python3\n").map(|s| s.name.as_str()),
            Some("Python")
        );
        assert!(find_syntax("notes.unknownext", "hello").is_none());
        let p = highlight("a.txt", "x < y\n");
        assert_eq!(p.lines, vec!["x &lt; y"]);
        assert!(!p.highlighted);
    }

    #[test]
    fn css_has_both_themes() {
        let css = css();
        assert!(css.contains(".hl-code"));
        assert!(css.contains("prefers-color-scheme: dark"));
        assert!(css.contains("[data-theme=\"dark\"] .hl-"));
    }
}
