//! GitHub Flavored Markdown rendering (comrak) with sanitization (ammonia).
//!
//! Supports tables, strikethrough, autolinks, task lists, footnotes, heading
//! anchors, a safe subset of raw HTML, and GitHub references:
//! `@user` / `@org/team` mentions, `#123` and `owner/repo#123` issue
//! references, and commit SHAs (7–40 hex chars containing a digit).
//!
//! Also (P35, kept in parity with the web client renderer
//! `web/src/ui/markdown/render.ts`, see `testdata/markdown/`): `GH-123`
//! references, repository custom autolinks ([`AutolinkRule`]), gemoji
//! `:shortcodes:` (shared table `web/src/ui/markdown/emoji.json`),
//! issue/commit URL shortening, math markers (`$…$`, `$$…$$`,
//! ```` ```math ````), lazy images and the camo image proxy
//! ([`crate::camo`]).
//!
//! References are linked syntactically (no existence checks); callers that
//! need validated links can post-process or extend [`RenderContext`].

use std::borrow::Cow;
use std::collections::HashMap;
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
    /// Repository custom autolinks (`JIRA-123` → URL), see
    /// [`load_autolinks`].
    pub autolinks: &'a [AutolinkRule],
}

/// A repository autolink reference (`repo_autolinks` row).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, sqlx::FromRow)]
pub struct AutolinkRule {
    pub key_prefix: String,
    /// Contains `<num>`, replaced by the reference id.
    pub url_template: String,
    /// Ids are `[A-Za-z0-9]+` (else digits only).
    pub is_alphanumeric: bool,
}

/// Autolinks of `repo_ids`, batched (one query).
pub async fn load_autolinks(
    db: &sqlx::PgPool,
    repo_ids: &[i64],
) -> Result<HashMap<i64, Vec<AutolinkRule>>, sqlx::Error> {
    let mut out: HashMap<i64, Vec<AutolinkRule>> = HashMap::new();
    if repo_ids.is_empty() {
        return Ok(out);
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        repo_id: i64,
        #[sqlx(flatten)]
        rule: AutolinkRule,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT repo_id, key_prefix, url_template, is_alphanumeric FROM repo_autolinks \
         WHERE repo_id = ANY($1) ORDER BY repo_id, length(key_prefix) DESC, id",
    )
    .bind(repo_ids)
    .fetch_all(db)
    .await?;
    for r in rows {
        out.entry(r.repo_id).or_default().push(r.rule);
    }
    Ok(out)
}

/// Autolinks of one repository (empty on error: rendering never fails).
pub async fn repo_autolinks(db: &sqlx::PgPool, repo_id: i64) -> Vec<AutolinkRule> {
    load_autolinks(db, &[repo_id])
        .await
        .map(|mut m| m.remove(&repo_id).unwrap_or_default())
        .unwrap_or_default()
}

/// gemoji shortcode → emoji, shared with the web client.
static EMOJI: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../web/src/ui/markdown/emoji.json"))
        .expect("emoji.json is valid")
});

/// The emoji for a `:shortcode:` name.
pub fn emoji(name: &str) -> Option<&'static str> {
    EMOJI.get(name).map(String::as_str)
}

/// All shortcodes (e.g. for `GET /emojis`).
pub fn emoji_names() -> impl Iterator<Item = &'static str> {
    EMOJI.keys().map(String::as_str)
}

impl<'a> RenderContext<'a> {
    pub fn new(base_url: &'a str) -> Self {
        Self {
            base_url,
            repo: None,
            references: true,
            autolinks: &[],
        }
    }

    pub fn with_autolinks(mut self, autolinks: &'a [AutolinkRule]) -> Self {
        self.autolinks = autolinks;
        self
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
    o.extension.math_dollars = true;
    o.extension.math_code = true;
    o.extension.header_id_prefix = Some(String::new());
    o.parse.relaxed_tasklist_matching = true;
    o.render.github_pre_lang = true;
    o.render.tasklist_classes = true;
    // Raw HTML is passed through and then sanitized by ammonia below.
    o.render.r#unsafe = true;
    o
}

static OPTIONS: LazyLock<Options<'static>> = LazyLock::new(options);

static SANITIZER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
    let mut b = ammonia::Builder::default();
    b.add_tags(["input", "section", "picture", "source", "g-emoji"])
        .add_tag_attributes("g-emoji", ["class", "alias"])
        .add_tag_attributes("span", ["data-math-style"])
        .add_tag_attributes("code", ["data-math-style"])
        .add_tag_attributes("input", ["type", "checked", "disabled", "class"])
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
                | "g-emoji"
                | "autolink"
        )
}

/// Render GFM to sanitized HTML, linking references per `ctx`.
pub fn render(text: &str, ctx: &RenderContext<'_>) -> String {
    let arena = Arena::new();
    let root = parse_document(&arena, text, &OPTIONS);
    link_references(&arena, root, ctx);
    let mut html = String::new();
    if format_html(root, &OPTIONS, &mut html).is_err() {
        return String::new();
    }
    finish(&sanitize(&html), ctx.base_url)
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
    link_references(&arena, root, ctx);
    let mut html = String::new();
    if format_html(root, &OPTIONS, &mut html).is_err() {
        return String::new();
    }
    finish(&sanitize(&html), ctx.base_url)
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

/// `@user` and `@org/team` mentions found in GFM text (outside code spans,
/// code blocks and links), deduplicated case-insensitively, in order of
/// first appearance. Logins are returned as written; resolve them with a
/// case-insensitive lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mentions {
    pub users: Vec<String>,
    /// `(org, team_slug)` pairs.
    pub teams: Vec<(String, String)>,
}

/// Extract mentions from `text` (see [`Mentions`]).
pub fn mentions(text: &str) -> Mentions {
    let mut out = Mentions::default();
    if !text.contains('@') {
        return out;
    }
    let arena = Arena::new();
    let root = parse_document(&arena, text, &OPTIONS);
    let ctx = RenderContext::new("");
    for node in root.descendants() {
        let NodeValue::Text(t) = &node.data.borrow().value else {
            continue;
        };
        if inside_link_or_code(node) {
            continue;
        }
        for seg in scan(t, &ctx) {
            let Segment::Link { text, class, .. } = seg else {
                continue;
            };
            let name = text.trim_start_matches('@');
            match class {
                "user-mention" => {
                    if !out.users.iter().any(|u| u.eq_ignore_ascii_case(name)) {
                        out.users.push(name.to_string());
                    }
                }
                "team-mention" => {
                    if let Some((org, team)) = name.split_once('/') {
                        let dup = out.teams.iter().any(|(o, t)| {
                            o.eq_ignore_ascii_case(org) && t.eq_ignore_ascii_case(team)
                        });
                        if !dup {
                            out.teams.push((org.to_string(), team.to_string()));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
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
    if ctx.references {
        shorten_urls(arena, root, ctx);
    }
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
                Segment::Emoji { name, emoji } => NodeValue::HtmlInline(format!(
                    "<g-emoji class=\"g-emoji\" alias=\"{}\">{emoji}</g-emoji>",
                    escape_html(&name)
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
    Emoji {
        name: String,
        emoji: &'static str,
    },
}

/// Autolinked bare URLs to issues, pull requests and commits on this
/// instance get GitHub's short text: `#12` / `owner/repo#12` (plus
/// ` (comment)` for comment anchors), `abc1234` / `owner/repo@abc1234`.
fn shorten_urls<'a>(arena: &'a Arena<'a>, root: &'a AstNode<'a>, ctx: &RenderContext<'_>) {
    let base = ctx.base_url.trim_end_matches('/');
    if base.is_empty() {
        return;
    }
    let links: Vec<&'a AstNode<'a>> = root
        .descendants()
        .filter(|n| matches!(n.data.borrow().value, NodeValue::Link(_)))
        .collect();
    for node in links {
        let url = match &node.data.borrow().value {
            NodeValue::Link(l) => l.url.to_string(),
            _ => continue,
        };
        let text: String = node
            .children()
            .map(|c| match &c.data.borrow().value {
                NodeValue::Text(t) => t.to_string(),
                _ => "\0".to_string(),
            })
            .collect();
        if text != url {
            continue;
        }
        let Some((short, class)) = short_url(base, ctx.repo, &url) else {
            continue;
        };
        node.insert_before(
            arena.alloc(
                NodeValue::HtmlInline(format!(
                    "<a class=\"{class}\" href=\"{}\">{}</a>",
                    escape_html(&url),
                    escape_html(&short)
                ))
                .into(),
            ),
        );
        node.detach();
    }
}

/// Short text and class for an instance URL (see [`shorten_urls`]).
fn short_url(base: &str, repo: Option<(&str, &str)>, url: &str) -> Option<(String, &'static str)> {
    let rest = url.strip_prefix(base)?.strip_prefix('/')?;
    let (path, frag) = match rest.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (rest, None),
    };
    if path.contains('?') {
        return None;
    }
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let [owner, name, kind, id] = parts.as_slice() else {
        return None;
    };
    let same =
        repo.is_some_and(|(o, r)| o.eq_ignore_ascii_case(owner) && r.eq_ignore_ascii_case(name));
    let prefix = if same {
        String::new()
    } else {
        format!("{owner}/{name}")
    };
    match *kind {
        "issues" | "pull" if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
            let comment = frag
                .is_some_and(|f| f.starts_with("issuecomment-") || f.starts_with("discussion_r"));
            if frag.is_some() && !comment {
                return None;
            }
            let suffix = if comment { " (comment)" } else { "" };
            Some((format!("{prefix}#{id}{suffix}"), "issue-link"))
        }
        "commit"
            if frag.is_none()
                && (7..=40).contains(&id.len())
                && id.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            let sha = &id[..7];
            let text = if same {
                sha.to_string()
            } else {
                format!("{prefix}@{sha}")
            };
            Some((text, "commit-link"))
        }
        _ => None,
    }
}

/// Post-process sanitized HTML: footnote links point at the
/// `user-content-` prefixed ids, images get `loading="lazy"
/// decoding="async"` and external `src` go through the camo proxy when
/// enabled.
fn finish(html: &str, base: &str) -> String {
    let html = if html.contains("href=\"#fn") {
        rewrite_urls(html, |href, kind| {
            (kind == UrlAttr::Href && (href.starts_with("#fn-") || href.starts_with("#fnref-")))
                .then(|| format!("#user-content-{}", &href[1..]))
        })
    } else {
        html.to_string()
    };
    if !html.contains("<img") {
        return html;
    }
    let camo = crate::camo::enabled() && !base.is_empty();
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html.as_str();
    while let Some(at) = rest.find("<img") {
        let tag_end = rest[at..].find('>').map_or(rest.len(), |e| at + e + 1);
        out.push_str(&rest[..at]);
        let tag = &rest[at..tag_end];
        let tag = if camo {
            rewrite_urls(tag, |src, kind| {
                (kind == UrlAttr::Src && crate::camo::is_external(base, src))
                    .then(|| crate::camo::url(base, src))
            })
        } else {
            tag.to_string()
        };
        out.push_str("<img loading=\"lazy\" decoding=\"async\"");
        out.push_str(&tag[4..]);
        rest = &rest[tag_end..];
    }
    out.push_str(rest);
    out
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
    let refs = ctx.references;
    while i < b.len() {
        let prev_ok = i == 0 || !is_word(b[i - 1]) && b[i - 1] != b'/' && b[i - 1] != b'@';
        // :emoji:
        if b[i] == b':' {
            let mut j = i + 1;
            while j < b.len() && (is_word(b[j]) || b[j] == b'+' || b[j] == b'-') {
                j += 1;
            }
            if j > i + 1
                && j < b.len()
                && b[j] == b':'
                && let Some(e) = emoji(&text[i + 1..j])
            {
                push(
                    &mut out,
                    &mut plain_start,
                    i,
                    j + 1,
                    Segment::Emoji {
                        name: text[i + 1..j].to_string(),
                        emoji: e,
                    },
                );
                i = j + 1;
                continue;
            }
        }
        // custom autolinks (longest prefix first, see `load_autolinks`)
        if refs && prev_ok && !ctx.autolinks.is_empty() {
            let hit = ctx.autolinks.iter().find_map(|rule| {
                let p = rule.key_prefix.as_bytes();
                let end = i + p.len();
                if p.is_empty() || end >= b.len() || !b[i..end].eq_ignore_ascii_case(p) {
                    return None;
                }
                let mut j = end;
                while j < b.len()
                    && (b[j].is_ascii_digit()
                        || rule.is_alphanumeric && b[j].is_ascii_alphanumeric())
                {
                    j += 1;
                }
                (j > end && (j == b.len() || !is_word(b[j])))
                    .then(|| (j, rule.url_template.replace("<num>", &text[end..j])))
            });
            if let Some((j, url)) = hit {
                push(
                    &mut out,
                    &mut plain_start,
                    i,
                    j,
                    Segment::Link {
                        url,
                        text: text[i..j].to_string(),
                        class: "autolink",
                    },
                );
                i = j;
                continue;
            }
        }
        // @mention or @org/team
        if refs && b[i] == b'@' && prev_ok && i + 1 < b.len() && b[i + 1].is_ascii_alphanumeric() {
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
        if refs && prev_ok && b[i].is_ascii_alphanumeric() {
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
        if let Some((owner, repo)) = ctx.repo.filter(|_| refs) {
            // GH-123
            if prev_ok && b[i..].starts_with(b"GH-") && i + 3 < b.len() && b[i + 3].is_ascii_digit()
            {
                let mut j = i + 3;
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
                            url: format!("{base}/{owner}/{repo}/issues/{}", &text[i + 3..j]),
                            text: text[i..j].to_string(),
                            class: "issue-link",
                        },
                    );
                    i = j;
                    continue;
                }
            }
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
    if let Some(n) = text.strip_prefix("GH-") {
        return Some(IssueRef {
            owner: None,
            repo: None,
            number: n.parse().ok()?,
        });
    }
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

/// Which attribute a URL passed to [`rewrite_urls`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlAttr {
    /// `href` (links)
    Href,
    /// `src` (images, media)
    Src,
}

/// Rewrite `href` / `src` attribute values in (sanitized) HTML, e.g. to
/// resolve relative links in a rendered README against the repository.
/// `f` receives the unescaped URL and returns a replacement, or `None` to
/// keep it. Only attributes inside tags are touched (never text).
pub fn rewrite_urls(html: &str, f: impl Fn(&str, UrlAttr) -> Option<String>) -> String {
    let b = html.as_bytes();
    let mut out = String::with_capacity(html.len() + 64);
    let mut i = 0;
    let mut copied = 0;
    let mut in_tag = false;
    while i < b.len() {
        let c = b[i];
        if !in_tag {
            if c == b'<' {
                in_tag = true;
            }
            i += 1;
            continue;
        }
        match c {
            b'>' => {
                in_tag = false;
                i += 1;
            }
            b'"' | b'\'' => {
                // Quoted value not preceded by href=/src= (handled below).
                let end = html[i + 1..].find(c as char).map_or(b.len(), |e| i + 1 + e);
                i = end + 1;
            }
            b' ' | b'\t' | b'\n' => {
                let rest = &html[i + 1..];
                let attr = if rest.starts_with("href=\"") {
                    Some((UrlAttr::Href, 6))
                } else if rest.starts_with("src=\"") {
                    Some((UrlAttr::Src, 5))
                } else {
                    None
                };
                match attr {
                    Some((kind, len)) => {
                        let start = i + 1 + len;
                        let end = html[start..].find('"').map_or(b.len(), |e| start + e);
                        let raw = &html[start..end];
                        let value = raw.replace("&quot;", "\"").replace("&amp;", "&");
                        if let Some(new) = f(&value, kind) {
                            out.push_str(&html[copied..start]);
                            out.push_str(&escape_html(&new));
                            copied = end;
                        }
                        i = end + 1;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }
    out.push_str(&html[copied.min(html.len())..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RenderContext<'static> {
        RenderContext::new("http://h").with_repo("o", "r")
    }

    /// `testdata/markdown/*.md` → `*.html` snapshots shared with the web
    /// client's parity test (see `testdata/markdown/README.md`).
    #[test]
    fn golden_corpus() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/markdown");
        let rules = [AutolinkRule {
            key_prefix: "JIRA-".into(),
            url_template: "https://jira.example/browse/<num>".into(),
            is_alphanumeric: true,
        }];
        let ctx = RenderContext::new("https://bgh.example")
            .with_repo("octo", "demo")
            .with_autolinks(&rules);
        let update = std::env::var_os("BGH_UPDATE_GOLDEN").is_some();
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .filter(|p| p.file_stem().is_some_and(|s| s != "README"))
            .collect();
        names.sort();
        assert!(names.len() >= 6, "corpus missing in {}", dir.display());
        for md in names {
            let html = render(&std::fs::read_to_string(&md).unwrap(), &ctx);
            let golden = md.with_extension("html");
            if update {
                std::fs::write(&golden, &html).unwrap();
                continue;
            }
            let want = std::fs::read_to_string(&golden).unwrap_or_default();
            assert_eq!(
                html,
                want,
                "{} (BGH_UPDATE_GOLDEN=1 to refresh)",
                md.display()
            );
        }
    }

    #[test]
    fn proxies_external_images_and_keeps_emoji_without_references() {
        let html = render("![a](https://img.example/a.png) ![b](http://h/x.png)", &ctx());
        assert!(html.contains("src=\"http://h/_bgh/camo/"), "{html}");
        assert!(html.contains("src=\"http://h/x.png\""), "{html}");
        assert_eq!(html.matches("loading=\"lazy\" decoding=\"async\"").count(), 2);
        let mut plain = ctx();
        plain.references = false;
        let html = render("#1 @a :tada:", &plain);
        assert!(!html.contains("<a"), "{html}");
        assert!(html.contains("alias=\"tada\""), "{html}");
        assert!(emoji_names().count() > 1800);
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
    fn extracts_mentions() {
        let m = mentions(
            "hi @alice and @Bob, cc @acme/core `@notme` @alice\n\n```\n@code\n```\nmail a@b.com [@link](http://x)",
        );
        assert_eq!(m.users, vec!["alice".to_string(), "Bob".to_string()]);
        assert_eq!(m.teams, vec![("acme".to_string(), "core".to_string())]);
        assert_eq!(mentions("no mentions"), Mentions::default());
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

    #[test]
    fn rewrites_urls_in_tags_only() {
        let html = "<p><a href=\"docs/a.md\">x</a> <img src=\"img.png\" alt=\"a src=&quot;q\"> src=\"text\"</p><a href=\"https://x.y/?a=1&amp;b=2\">y</a>";
        let out = rewrite_urls(html, |u, kind| {
            (!u.starts_with("https://")).then(|| {
                format!(
                    "/base/{}/{u}",
                    if kind == UrlAttr::Src { "raw" } else { "blob" }
                )
            })
        });
        assert_eq!(
            out,
            "<p><a href=\"/base/blob/docs/a.md\">x</a> <img src=\"/base/raw/img.png\" alt=\"a src=&quot;q\"> src=\"text\"</p><a href=\"https://x.y/?a=1&amp;b=2\">y</a>"
        );
    }
}
