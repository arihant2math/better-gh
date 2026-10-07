//! Server-side syntax highlighting (syntect grammars + two-face's extra
//! syntaxes, Oniguruma engine) into per-line HTML with a small fixed class
//! set shared with the web client (docs/SYNC_PROTOCOL.md §10):
//!
//! `hl-k` keyword · `hl-s` string · `hl-c` comment · `hl-n` number/constant ·
//! `hl-t` type · `hl-f` function/macro · `hl-a` attribute/tag
//!
//! Spans are flat (never nested) and never cross a line, so every line is
//! self-contained HTML even inside multi-line comments or strings. Colors
//! come from the client's CSS tokens, so one rendering serves both themes.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use syntect::parsing::{
    BasicScopeStackOp, ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet,
};
use syntect::util::LinesWithEndings;

/// Files larger than this are not highlighted.
pub const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
/// Files with more lines than this are not highlighted.
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
    /// Lowercase language id (`rust`, `typescript`, `c++`...); `None` for
    /// plain text.
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

/// Language id of a grammar: its lowercased name.
pub fn language_id(syntax: &SyntaxReference) -> String {
    syntax.name.to_lowercase()
}

/// HTML-escape for text nodes.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    push_escaped(&mut out, s);
    out
}

fn push_escaped(out: &mut String, s: &str) {
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
}

/// Escaped, unhighlighted lines.
pub fn plain(text: &str) -> Vec<String> {
    text.lines().map(escape).collect()
}

/// Class for one scope, or `None` to keep looking at enclosing scopes.
fn scope_class(scope: &Scope) -> Option<Option<char>> {
    let name = scope.build_string();
    let has = |p: &str| name == p || name.starts_with(&format!("{p}."));
    // Operators and punctuation take the class of what encloses them
    // (string quotes stay strings, `+` stays uncolored).
    if has("keyword.operator") || has("punctuation") {
        return None;
    }
    let class = if has("comment") {
        'c'
    } else if has("string") || has("constant.character") {
        's'
    } else if has("entity.name.function")
        || has("support.function")
        || has("variable.function")
        || has("support.macro")
        || has("entity.name.macro")
    {
        'f'
    } else if has("entity.name.type")
        || has("entity.name.class")
        || has("entity.name.struct")
        || has("entity.name.enum")
        || has("entity.name.trait")
        || has("entity.name.interface")
        || has("entity.other.inherited-class")
        || has("support.type")
        || has("support.class")
        || has("storage.type.primitive")
        || has("storage.type.numeric")
    {
        't'
    } else if has("entity.other.attribute-name")
        || has("entity.name.tag")
        || has("meta.attribute")
        || has("meta.annotation")
        || has("variable.annotation")
    {
        'a'
    } else if has("constant") || has("support.constant") {
        'n'
    } else if has("keyword") || has("storage") || has("variable.language") {
        'k'
    } else {
        return None;
    };
    Some(Some(class))
}

/// Primitive type names that some grammars scope like keywords
/// (`storage.type` for both `let` and `u32` in Rust).
const PRIMITIVES: &[&str] = &[
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f32", "f64", "int", "float", "double", "long", "short", "void", "byte",
    "string", "boolean", "uint", "rune",
];

fn class_of(stack: &ScopeStack, text: &str) -> Option<char> {
    let class = stack
        .as_slice()
        .iter()
        .rev()
        .find_map(scope_class)
        .flatten();
    if class == Some('k') && PRIMITIVES.contains(&text.trim()) {
        return Some('t');
    }
    class
}

/// Append `text` with `class`, merging with an open span of the same class.
struct LineBuf {
    html: String,
    open: Option<char>,
}

impl LineBuf {
    fn push(&mut self, class: Option<char>, text: &str) {
        let text = text.trim_end_matches(['\n', '\r']);
        if text.is_empty() {
            return;
        }
        if self.open != class {
            if self.open.is_some() {
                self.html.push_str("</span>");
            }
            if let Some(c) = class {
                self.html.push_str("<span class=\"hl-");
                self.html.push(c);
                self.html.push_str("\">");
            }
            self.open = class;
        }
        push_escaped(&mut self.html, text);
    }

    fn finish(mut self) -> String {
        if self.open.is_some() {
            self.html.push_str("</span>");
        }
        self.html
    }
}

/// Highlight a Markdown fenced code block by its info-string language
/// (`rust`, `ts`, `Python`...); plain text when no grammar matches.
pub fn highlight_lang(lang: &str, text: &str) -> Highlighted {
    let ext = SYNTAXES
        .find_syntax_by_token(lang.trim())
        .filter(|s| s.name != "Plain Text")
        .and_then(|s| s.file_extensions.first());
    match ext {
        Some(ext) => highlight(&format!("snippet.{ext}"), text),
        None => Highlighted {
            language: None,
            lines: plain(text),
            highlighted: false,
        },
    }
}

/// Highlight `text` (the contents of `path`).
pub fn highlight(path: &str, text: &str) -> Highlighted {
    let syntax = find_syntax(path, text);
    let language = syntax.map(language_id);
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
        let content = line.trim_end_matches(['\n', '\r']);
        if highlighted && started.elapsed() > TIME_BUDGET {
            highlighted = false;
        }
        if !highlighted {
            lines.push(escape(content));
            continue;
        }
        let Ok(ops) = state.parse_line(line, &SYNTAXES) else {
            highlighted = false;
            lines.push(escape(content));
            continue;
        };
        let mut buf = LineBuf {
            html: String::with_capacity(line.len() * 2),
            open: None,
        };
        let mut pos = 0;
        let mut ok = true;
        for (i, op) in &ops {
            let i = (*i).min(line.len());
            if i > pos {
                buf.push(class_of(&stack, &line[pos..i]), &line[pos..i]);
                pos = i;
            }
            if stack
                .apply_with_hook(op, |_: BasicScopeStackOp, _| {})
                .is_err()
            {
                ok = false;
                break;
            }
        }
        if !ok {
            highlighted = false;
            lines.push(escape(content));
            continue;
        }
        if pos < line.len() {
            buf.push(class_of(&stack, &line[pos..]), &line[pos..]);
        }
        lines.push(buf.finish());
    }
    Highlighted {
        language,
        lines,
        highlighted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_per_line() {
        let src = "/* multi\nline */\n#[derive(Debug)]\nfn main() {\n    let s: u32 = 42; println!(\"a<b\");\n}\n";
        let h = highlight("src/main.rs", src);
        assert_eq!(h.language.as_deref(), Some("rust"));
        assert!(h.highlighted);
        assert_eq!(h.lines.len(), 6);
        for l in &h.lines {
            assert_eq!(
                l.matches("<span").count(),
                l.matches("</span>").count(),
                "{l}"
            );
            assert!(!l.contains('\n'));
        }
        assert_eq!(h.lines[0], "<span class=\"hl-c\">/* multi</span>");
        assert_eq!(h.lines[1], "<span class=\"hl-c\">line */</span>");
        assert!(h.lines[2].contains("hl-a"), "{}", h.lines[2]);
        assert!(
            h.lines[3].starts_with("<span class=\"hl-k\">fn</span>"),
            "{}",
            h.lines[3]
        );
        assert!(
            h.lines[3].contains("<span class=\"hl-f\">main</span>"),
            "{}",
            h.lines[3]
        );
        let l4 = &h.lines[4];
        assert!(l4.contains("<span class=\"hl-t\">u32</span>"), "{l4}");
        assert!(l4.contains("<span class=\"hl-n\">42</span>"), "{l4}");
        assert!(
            l4.contains("<span class=\"hl-s\">&quot;a&lt;b&quot;</span>"),
            "{l4}"
        );
        assert!(l4.contains("hl-f\">println!"), "{l4}");
        // Only the documented classes.
        for l in &h.lines {
            for part in l.split("class=\"hl-").skip(1) {
                assert!("kscntfa".contains(&part[..1]), "{l}");
            }
        }
    }

    #[test]
    fn detects_languages() {
        assert_eq!(
            find_syntax("web/app.tsx", "").map(language_id).as_deref(),
            Some("typescriptreact")
        );
        assert_eq!(
            find_syntax("Dockerfile", "").map(language_id).as_deref(),
            Some("dockerfile")
        );
        assert_eq!(
            find_syntax("script", "#!/usr/bin/env python3\n")
                .map(language_id)
                .as_deref(),
            Some("python")
        );
        assert!(find_syntax("notes.unknownext", "hello").is_none());
        let p = highlight("a.txt", "x < y\n");
        assert_eq!(p.lines, vec!["x &lt; y"]);
        assert!(!p.highlighted);
    }
}
