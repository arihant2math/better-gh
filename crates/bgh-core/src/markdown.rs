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
                | "wiki-link"
                | "wiki-missing"
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

/// Link resolution hook for [`render_with_links`] (used by wikis).
pub trait LinkResolver {
    /// Resolve a `[[target]]` / `[[text|target]]` wiki link to
    /// `(href, class)`; `None` leaves the brackets as plain text.
    fn wiki_link(&self, target: &str) -> Option<(String, &'static str)>;
    /// Rewrite a Markdown link destination (e.g. a relative link) to
    /// `(href, class)`; `None` keeps the link unchanged.
    fn rewrite_link(&self, url: &str) -> Option<(String, &'static str)>;
}

/// Like [`render`], additionally resolving `[[wiki links]]` and rewriting
/// link destinations through `links`. Resolved links carry the returned
/// class (only sanitizer-allowed classes such as `wiki-link` /
/// `wiki-missing` survive).
pub fn render_with_links(text: &str, ctx: &RenderContext<'_>, links: &dyn LinkResolver) -> String {
    let arena = Arena::new();
    let root = parse_document(&arena, text, &OPTIONS);
    merge_text_nodes(root);
    link_wiki_pages(&arena, root, links);
    rewrite_links(&arena, root, links);
    if ctx.references {
        link_references(&arena, root, ctx);
    }
    let mut html = String::new();
    if format_html(root, &OPTIONS, &mut html).is_err() {
        return String::new();
    }
    sanitize(&html)
}

fn is_text(n: &AstNode<'_>) -> bool {
    matches!(n.data.borrow().value, NodeValue::Text(_))
}

/// Join adjacent text siblings (the parser splits text at brackets).
fn merge_text_nodes<'a>(root: &'a AstNode<'a>) {
    let parents: Vec<&'a AstNode<'a>> = root.descendants().collect();
    for parent in parents {
        let mut child = parent.first_child();
        while let Some(c) = child {
            if is_text(c) {
                while let Some(next) = c.next_sibling().filter(|n| is_text(n)) {
                    let mut merged = match &c.data.borrow().value {
                        NodeValue::Text(t) => t.to_string(),
                        _ => String::new(),
                    };
                    if let NodeValue::Text(t) = &next.data.borrow().value {
                        merged.push_str(t);
                    }
                    c.data.borrow_mut().value = NodeValue::Text(merged.into());
                    next.detach();
                }
            }
            child = c.next_sibling();
        }
    }
}

fn anchor_open(href: &str, class: &str, title: Option<&str>) -> String {
    match title.filter(|t| !t.is_empty()) {
        Some(t) => format!(
            "<a class=\"{}\" href=\"{}\" title=\"{}\">",
            escape_html(class),
            escape_html(href),
            escape_html(t)
        ),
        None => format!(
            "<a class=\"{}\" href=\"{}\">",
            escape_html(class),
            escape_html(href)
        ),
    }
}

fn link_wiki_pages<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>, links: &dyn LinkResolver) {
    let text_nodes: Vec<&'a AstNode<'a>> = root
        .descendants()
        .filter(|n| is_text(n) && !inside_link_or_code(n))
        .collect();
    for node in text_nodes {
        let text = match &node.data.borrow().value {
            NodeValue::Text(t) => t.to_string(),
            _ => continue,
        };
        if !text.contains("[[") {
            continue;
        }
        let mut values: Vec<NodeValue> = Vec::new();
        let mut rest = text.as_str();
        let mut changed = false;
        while let Some(start) = rest.find("[[") {
            let after = &rest[start + 2..];
            let Some(end) = after.find("]]") else { break };
            let inner = &after[..end];
            let (label, target) = match inner.split_once('|') {
                Some((l, t)) => (l.trim(), t.trim()),
                None => (inner.trim(), inner.trim()),
            };
            let resolved = (!target.is_empty() && !inner.contains('['))
                .then(|| links.wiki_link(target))
                .flatten();
            let Some((href, class)) = resolved else {
                // Not a wiki link: keep `[[` verbatim and continue after it.
                values.push(NodeValue::Text(rest[..start + 2].to_string().into()));
                rest = &rest[start + 2..];
                continue;
            };
            changed = true;
            if start > 0 {
                values.push(NodeValue::Text(rest[..start].to_string().into()));
            }
            let label = if label.is_empty() { target } else { label };
            values.push(NodeValue::HtmlInline(format!(
                "{}{}</a>",
                anchor_open(&href, class, None),
                escape_html(label)
            )));
            rest = &after[end + 2..];
        }
        if !changed {
            continue;
        }
        if !rest.is_empty() {
            values.push(NodeValue::Text(rest.to_string().into()));
        }
        for value in values {
            node.insert_before(arena.alloc(value.into()));
        }
        node.detach();
    }
}

fn rewrite_links<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>, links: &dyn LinkResolver) {
    let link_nodes: Vec<&'a AstNode<'a>> = root
        .descendants()
        .filter(|n| matches!(n.data.borrow().value, NodeValue::Link(_)))
        .collect();
    for node in link_nodes {
        let (url, title) = match &node.data.borrow().value {
            NodeValue::Link(l) => (l.url.to_string(), l.title.to_string()),
            _ => continue,
        };
        let Some((href, class)) = links.rewrite_link(&url) else {
            continue;
        };
        let open = anchor_open(&href, class, Some(&title));
        node.insert_before(arena.alloc(NodeValue::HtmlInline(open).into()));
        let children: Vec<&'a AstNode<'a>> = node.children().collect();
        for child in children {
            node.insert_before(child);
        }
        node.insert_before(arena.alloc(NodeValue::HtmlInline("</a>".into()).into()));
        node.detach();
    }
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

/// An issue reference found in text: `#12` (`owner`/`repo` = `None`) or
/// `owner/repo#12` / a full issue or pull request URL on this instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IssueRef {
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub number: i64,
}

/// References found in Markdown text (outside code and existing links).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct References {
    /// `@login` mentions, deduplicated, in order of appearance.
    pub mentions: Vec<String>,
    /// `@org/team` mentions as `(org, team_slug)`.
    pub team_mentions: Vec<(String, String)>,
    /// Issue references, deduplicated.
    pub issues: Vec<IssueRef>,
}

/// Extract `@mentions`, team mentions and issue references from Markdown,
/// with the same syntax rules as [`render`] (ignoring code spans/blocks).
/// Links to `{base_url}/{owner}/{repo}/issues|pull/{n}` count as issue
/// references too.
pub fn extract_references(text: &str, base_url: &str) -> References {
    let arena = Arena::new();
    let root = parse_document(&arena, text, &OPTIONS);
    // A placeholder repo enables `#123` detection; resolved by the caller.
    let ctx = RenderContext::new(base_url).with_repo("\0", "\0");
    let mut out = References::default();
    let base = base_url.trim_end_matches('/');
    let push_issue = |out: &mut References, r: IssueRef| {
        if !out.issues.contains(&r) {
            out.issues.push(r);
        }
    };
    for node in root.descendants() {
        let value = node.data.borrow().value.clone();
        match value {
            NodeValue::Text(t) if !inside_link_or_code(node) => {
                for seg in scan(&t, &ctx) {
                    let Segment::Link { text, class, .. } = seg else {
                        continue;
                    };
                    match class {
                        "user-mention" => {
                            let login = text.trim_start_matches('@').to_string();
                            if !out.mentions.iter().any(|m| m.eq_ignore_ascii_case(&login)) {
                                out.mentions.push(login);
                            }
                        }
                        "team-mention" => {
                            if let Some((org, team)) = text.trim_start_matches('@').split_once('/')
                            {
                                let t = (org.to_string(), team.to_string());
                                if !out.team_mentions.contains(&t) {
                                    out.team_mentions.push(t);
                                }
                            }
                        }
                        "issue-link" => {
                            if let Some(r) = parse_issue_ref(&text) {
                                push_issue(&mut out, r);
                            }
                        }
                        _ => {}
                    }
                }
            }
            NodeValue::Link(link) => {
                if let Some(rest) = link.url.strip_prefix(base) {
                    let parts: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
                    if parts.len() == 4
                        && (parts[2] == "issues" || parts[2] == "pull")
                        && let Ok(number) = parts[3]
                            .split(['#', '?'])
                            .next()
                            .unwrap_or("")
                            .parse::<i64>()
                    {
                        push_issue(
                            &mut out,
                            IssueRef {
                                owner: Some(parts[0].to_string()),
                                repo: Some(parts[1].to_string()),
                                number,
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn parse_issue_ref(text: &str) -> Option<IssueRef> {
    let (path, num) = text.rsplit_once('#')?;
    let number = num.parse().ok()?;
    if path.is_empty() {
        return Some(IssueRef {
            owner: None,
            repo: None,
            number,
        });
    }
    let (owner, repo) = path.split_once('/')?;
    Some(IssueRef {
        owner: Some(owner.to_string()),
        repo: Some(repo.to_string()),
        number,
    })
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

    struct Wiki;
    impl LinkResolver for Wiki {
        fn wiki_link(&self, target: &str) -> Option<(String, &'static str)> {
            let slug = target.replace(' ', "-");
            let class = if slug == "Home" {
                "wiki-link"
            } else {
                "wiki-link wiki-missing"
            };
            Some((format!("/o/r/wiki/{slug}"), class))
        }
        fn rewrite_link(&self, url: &str) -> Option<(String, &'static str)> {
            (!url.contains(':')).then(|| (format!("/o/r/wiki/{url}"), "wiki-link"))
        }
    }

    #[test]
    fn resolves_wiki_links() {
        let html = render_with_links(
            "See [[Home]], [[the docs|Some Page]] and [rel](Home \"t\") or [ext](https://x.y). `[[Code]]` [[ ]]",
            &ctx(),
            &Wiki,
        );
        assert!(
            html.contains(r#"<a class="wiki-link" href="/o/r/wiki/Home" rel="nofollow noopener noreferrer">Home</a>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a class="wiki-link wiki-missing" href="/o/r/wiki/Some-Page" rel="nofollow noopener noreferrer">the docs</a>"#),
            "{html}"
        );
        assert!(html.contains(r#"title="t""#), "{html}");
        assert!(html.contains(">rel</a>"), "{html}");
        assert!(html.contains(r#"href="https://x.y""#), "{html}");
        assert!(html.contains("<code>[[Code]]</code>"), "{html}");
        assert!(html.contains("[[ ]]"), "{html}");
    }

    #[test]
    fn extracts_references() {
        let r = extract_references(
            "Hi @alice and @Alice, see #12, o/r#3 and http://h.io/x/y/issues/9.\n\n`@bob #4`\n\n@org/team",
            "http://h.io",
        );
        assert_eq!(r.mentions, vec!["alice".to_string()]);
        assert_eq!(r.team_mentions, vec![("org".into(), "team".into())]);
        let nums: Vec<i64> = r.issues.iter().map(|i| i.number).collect();
        assert_eq!(nums, vec![12, 3, 9]);
        assert_eq!(r.issues[0].owner, None);
        assert_eq!(r.issues[1].owner.as_deref(), Some("o"));
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
