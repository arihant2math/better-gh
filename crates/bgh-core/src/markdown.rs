//! GitHub Flavored Markdown rendering (comrak) with sanitization (ammonia).
//!
//! Supports tables, strikethrough, autolinks, task lists, footnotes, heading
//! anchors, a safe subset of raw HTML, and GitHub references:
//! `@user` / `@org/team` mentions, `#123` and `owner/repo#123` issue
//! references, and commit SHAs (7–40 hex chars containing a digit).
//!
//! References are linked syntactically (no existence checks); callers that
//! need validated links can post-process or extend [`RenderContext`].

use std::borrow::Cow;
use std::sync::LazyLock;

use comrak::nodes::{AstNode, NodeValue};
use comrak::{Arena, Options, format_html, parse_document};

/// Context for resolving references.
#[derive(Debug, Clone, Copy)]
pub struct RenderContext<'a> {
    /// External base URL (`http://localhost:3000`), see `Config::base_url`.
    pub base_url: &'a str,
    /// Repository used to resolve `#123` and SHAs: `(owner, name)`.
    pub repo: Option<(&'a str, &'a str)>,
    /// Link `@mentions` and references (disable for e.g. commit messages
    /// rendered elsewhere).
    pub references: bool,
}

impl<'a> RenderContext<'a> {
    pub fn new(base_url: &'a str) -> Self {
        Self {
            base_url,
            repo: None,
            references: true,
        }
    }

    pub fn with_repo(mut self, owner: &'a str, name: &'a str) -> Self {
        self.repo = Some((owner, name));
        self
    }
}

fn options() -> Options<'static> {
    let mut o = Options::default();
    o.extension.strikethrough = true;
    o.extension.table = true;
    o.extension.autolink = true;
    o.extension.tasklist = true;
    o.extension.footnotes = true;
    o.extension.alerts = true;
    o.extension.header_id_prefix = Some(String::new());
    o.parse.relaxed_tasklist_matching = true;
    o.render.github_pre_lang = true;
    // Raw HTML is passed through and then sanitized by ammonia below.
    o.render.r#unsafe = true;
    o
}

static OPTIONS: LazyLock<Options<'static>> = LazyLock::new(options);

static SANITIZER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
    let mut b = ammonia::Builder::default();
    b.add_tags(["input", "section", "picture", "source", "g-emoji"])
        .add_tag_attributes("input", ["type", "checked", "disabled"])
        .add_tag_attributes("a", ["class", "id", "aria-hidden"])
        .add_tag_attributes("code", ["class"])
        .add_tag_attributes("pre", ["lang"])
        .add_tag_attributes("li", ["id", "class"])
        .add_tag_attributes("ul", ["class"])
        .add_tag_attributes("ol", ["class", "start"])
        .add_tag_attributes("section", ["class"])
        .add_tag_attributes("sup", ["class"])
        .add_tag_attributes("div", ["class"])
        .add_tag_attributes("p", ["class"])
        .add_tag_attributes("td", ["align"])
        .add_tag_attributes("th", ["align"])
        .add_tag_attributes("source", ["srcset", "media", "type"])
        .add_tag_attributes("h1", ["id"])
        .add_tag_attributes("h2", ["id"])
        .add_tag_attributes("h3", ["id"])
        .add_tag_attributes("h4", ["id"])
        .add_tag_attributes("h5", ["id"])
        .add_tag_attributes("h6", ["id"])
        .id_prefix(Some("user-content-"))
        .link_rel(Some("nofollow noopener noreferrer"))
        .attribute_filter(|_el, attr, value| {
            if attr != "class" {
                return Some(Cow::Borrowed(value));
            }
            let kept: Vec<&str> = value
                .split_whitespace()
                .filter(|c| allowed_class(c))
                .collect();
            (!kept.is_empty()).then(|| Cow::Owned(kept.join(" ")))
        });
    b
});

fn allowed_class(c: &str) -> bool {
    c.starts_with("language-")
        || c.starts_with("markdown-alert")
        || matches!(
            c,
            "anchor"
                | "user-mention"
                | "team-mention"
                | "issue-link"
                | "commit-link"
                | "task-list-item"
                | "contains-task-list"
                | "task-list-item-checkbox"
                | "footnotes"
                | "footnote-ref"
                | "footnote-backref"
        )
}

/// Render GFM to sanitized HTML, linking references per `ctx`.
pub fn render(text: &str, ctx: &RenderContext<'_>) -> String {
    let arena = Arena::new();
    let root = parse_document(&arena, text, &OPTIONS);
    if ctx.references {
        link_references(&arena, root, ctx);
    }
    let mut html = String::new();
    if format_html(root, &OPTIONS, &mut html).is_err() {
        return String::new();
    }
    sanitize(&html)
}

/// Sanitize arbitrary HTML with the same policy as [`render`].
pub fn sanitize(html: &str) -> String {
    SANITIZER.clean(html).to_string()
}

fn inside_link_or_code<'a>(node: &'a AstNode<'a>) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.data.borrow().value {
            NodeValue::Link(_)
            | NodeValue::Image(_)
            | NodeValue::Code(_)
            | NodeValue::CodeBlock(_) => {
                return true;
            }
            _ => {}
        }
        cur = n.parent();
    }
    false
}

fn link_references<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>, ctx: &RenderContext<'_>) {
    let text_nodes: Vec<&'a AstNode<'a>> = root
        .descendants()
        .filter(|n| matches!(n.data.borrow().value, NodeValue::Text(_)))
        .filter(|n| !inside_link_or_code(n))
        .collect();
    for node in text_nodes {
        let text = match &node.data.borrow().value {
            NodeValue::Text(t) => t.to_string(),
            _ => continue,
        };
        let segments = scan(&text, ctx);
        if segments.iter().all(|s| matches!(s, Segment::Text(_))) {
            continue;
        }
        for seg in segments {
            let value = match seg {
                Segment::Text(t) => NodeValue::Text(t.into()),
                Segment::Link { url, text, class } => NodeValue::HtmlInline(format!(
                    "<a class=\"{class}\" href=\"{}\">{}</a>",
                    escape_html(&url),
                    escape_html(&text)
                )),
            };
            let new = arena.alloc(value.into());
            node.insert_before(new);
        }
        node.detach();
    }
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[derive(Debug, PartialEq)]
enum Segment {
    Text(String),
    Link {
        url: String,
        text: String,
        class: &'static str,
    },
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_login_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

fn is_repo_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')
}

/// Split text into plain segments and reference links.
fn scan(text: &str, ctx: &RenderContext<'_>) -> Vec<Segment> {
    let b = text.as_bytes();
    let base = ctx.base_url;
    let mut out = Vec::new();
    let mut plain_start = 0;
    let mut i = 0;
    let push = |out: &mut Vec<Segment>,
                plain_start: &mut usize,
                start: usize,
                end: usize,
                seg: Segment| {
        if *plain_start < start {
            out.push(Segment::Text(text[*plain_start..start].to_string()));
        }
        out.push(seg);
        *plain_start = end;
    };
    while i < b.len() {
        let prev_ok = i == 0 || !is_word(b[i - 1]) && b[i - 1] != b'/' && b[i - 1] != b'@';
        // @mention or @org/team
        if b[i] == b'@' && prev_ok && i + 1 < b.len() && b[i + 1].is_ascii_alphanumeric() {
            let mut j = i + 1;
            while j < b.len() && is_login_char(b[j]) && j - i <= 39 {
                j += 1;
            }
            let login = text[i + 1..j].trim_end_matches('-');
            let j = i + 1 + login.len();
            // @org/team
            if j + 1 < b.len() && b[j] == b'/' && b[j + 1].is_ascii_alphanumeric() {
                let mut k = j + 1;
                while k < b.len() && (is_login_char(b[k]) || b[k] == b'_') {
                    k += 1;
                }
                if k == b.len() || !is_word(b[k]) {
                    let team = &text[j + 1..k];
                    push(
                        &mut out,
                        &mut plain_start,
                        i,
                        k,
                        Segment::Link {
                            url: format!("{base}/orgs/{login}/teams/{team}"),
                            text: text[i..k].to_string(),
                            class: "team-mention",
                        },
                    );
                    i = k;
                    continue;
                }
            }
            if j == b.len() || !is_word(b[j]) {
                push(
                    &mut out,
                    &mut plain_start,
                    i,
                    j,
                    Segment::Link {
                        url: format!("{base}/{login}"),
                        text: text[i..j].to_string(),
                        class: "user-mention",
                    },
                );
                i = j;
                continue;
            }
        }
        // owner/repo#123
        if prev_ok && b[i].is_ascii_alphanumeric() {
            let mut j = i;
            while j < b.len() && is_login_char(b[j]) {
                j += 1;
            }
            if j < b.len() && b[j] == b'/' {
                let mut k = j + 1;
                while k < b.len() && is_repo_char(b[k]) {
                    k += 1;
                }
                if k > j + 1 && k + 1 < b.len() && b[k] == b'#' && b[k + 1].is_ascii_digit() {
                    let mut m = k + 1;
                    while m < b.len() && b[m].is_ascii_digit() {
                        m += 1;
                    }
                    if m == b.len() || !is_word(b[m]) {
                        let (owner, repo, num) = (&text[i..j], &text[j + 1..k], &text[k + 1..m]);
                        push(
                            &mut out,
                            &mut plain_start,
                            i,
                            m,
                            Segment::Link {
                                url: format!("{base}/{owner}/{repo}/issues/{num}"),
                                text: text[i..m].to_string(),
                                class: "issue-link",
                            },
                        );
                        i = m;
                        continue;
                    }
                }
            }
        }
        if let Some((owner, repo)) = ctx.repo {
            // #123
            if b[i] == b'#' && prev_ok && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j == b.len() || !is_word(b[j]) {
                    push(
                        &mut out,
                        &mut plain_start,
                        i,
                        j,
                        Segment::Link {
                            url: format!("{base}/{owner}/{repo}/issues/{}", &text[i + 1..j]),
                            text: text[i..j].to_string(),
                            class: "issue-link",
                        },
                    );
                    i = j;
                    continue;
                }
            }
            // commit SHA
            if prev_ok && b[i].is_ascii_hexdigit() {
                let mut j = i;
                while j < b.len() && b[j].is_ascii_hexdigit() {
                    j += 1;
                }
                let len = j - i;
                let word_end = j == b.len() || !is_word(b[j]);
                let sha = &text[i..j];
                if (7..=40).contains(&len)
                    && word_end
                    && sha.bytes().any(|c| c.is_ascii_digit())
                    && sha.bytes().any(|c| c.is_ascii_alphabetic())
                {
                    push(
                        &mut out,
                        &mut plain_start,
                        i,
                        j,
                        Segment::Link {
                            url: format!(
                                "{base}/{owner}/{repo}/commit/{}",
                                sha.to_ascii_lowercase()
                            ),
                            text: sha[..7].to_string(),
                            class: "commit-link",
                        },
                    );
                    i = j;
                    continue;
                }
            }
        }
        // advance one UTF-8 char
        i += 1;
        while i < b.len() && !text.is_char_boundary(i) {
            i += 1;
        }
    }
    if plain_start < text.len() {
        out.push(Segment::Text(text[plain_start..].to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RenderContext<'static> {
        RenderContext::new("http://h").with_repo("o", "r")
    }

    #[test]
    fn renders_gfm() {
        let html = render(
            "# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n\n~~gone~~ https://example.com",
            &ctx(),
        );
        assert!(html.contains("<table>"), "{html}");
        assert!(html.contains("<del>gone</del>"), "{html}");
        assert!(html.contains("type=\"checkbox\""), "{html}");
        assert!(html.contains("href=\"https://example.com\""), "{html}");
        assert!(html.contains("id=\"user-content-title\""), "{html}");
    }

    #[test]
    fn sanitizes_html() {
        let html = render(
            "<script>alert(1)</script><b onclick=\"x\">hi</b><a href=\"javascript:x\">y</a>",
            &ctx(),
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("onclick"), "{html}");
        assert!(!html.contains("javascript:"), "{html}");
        assert!(html.contains("<b>hi</b>"), "{html}");
    }

    #[test]
    fn links_references() {
        let html = render(
            "Thanks @alice and @acme/core! Fixes #12, see other/repo#3 and deadbeef123. mail@example.com `#5` #x",
            &ctx(),
        );
        assert!(html.contains(r#"<a class="user-mention" href="http://h/alice" rel="nofollow noopener noreferrer">@alice</a>"#), "{html}");
        assert!(
            html.contains(r#"href="http://h/orgs/acme/teams/core""#),
            "{html}"
        );
        assert!(html.contains(r#"href="http://h/o/r/issues/12""#), "{html}");
        assert!(
            html.contains(r#"href="http://h/other/repo/issues/3""#),
            "{html}"
        );
        assert!(
            html.contains(r#"href="http://h/o/r/commit/deadbeef123""#),
            "{html}"
        );
        assert!(!html.contains("http://h/example"), "{html}");
        assert!(html.contains("<code>#5</code>"), "{html}");
        assert!(!html.contains("issues/5"), "{html}");
    }

    #[test]
    fn no_repo_means_no_issue_links() {
        let html = render("see #12", &RenderContext::new("http://h"));
        assert!(!html.contains("issue-link"), "{html}");
    }

    #[test]
    fn plain_words_are_not_shas() {
        let segs = scan("facade decade1 abcdefg", &ctx());
        assert!(
            matches!(
                &segs[..],
                [Segment::Text(_), Segment::Link { .. }, Segment::Text(_)]
            ),
            "{segs:?}"
        );
    }
}
