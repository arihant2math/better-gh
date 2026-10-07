//! Page rendering: Markdown with wiki links, other formats as `<pre>`.

use bgh_core::markdown::{self, LinkResolver, RenderContext};
use bgh_core::urls::encode_segment;

use crate::pages::{PageFile, find, parse_page_name, slug_of};

/// Resolves wiki links against the pages of one snapshot.
pub struct WikiLinks<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub pages: &'a [PageFile],
}

impl WikiLinks<'_> {
    fn page_href(&self, name: &str, fragment: &str) -> (String, &'static str) {
        let slug = slug_of(name);
        let (slug, class) = match find(self.pages, &slug) {
            Some(p) => (p.slug.clone(), "wiki-link"),
            None => (slug, "wiki-link wiki-missing"),
        };
        (
            format!(
                "/{}/{}/wiki/{}{fragment}",
                self.owner,
                self.repo,
                encode_segment(&slug)
            ),
            class,
        )
    }
}

fn has_scheme(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl LinkResolver for WikiLinks<'_> {
    fn wiki_link(&self, target: &str) -> Option<(String, &'static str)> {
        if has_scheme(target) {
            return Some((target.to_string(), "wiki-link"));
        }
        let (name, fragment) = match target.find('#') {
            Some(i) => (&target[..i], &target[i..]),
            None => (target, ""),
        };
        if name.trim().is_empty() || name.contains('/') {
            return None;
        }
        Some(self.page_href(name.trim(), fragment))
    }

    fn rewrite_link(&self, url: &str) -> Option<(String, &'static str)> {
        if url.is_empty()
            || url.starts_with(['#', '/', '?', '.'])
            || has_scheme(url)
            || url.contains('/')
        {
            return None;
        }
        let (name, fragment) = match url.find(['#', '?']) {
            Some(i) => (&url[..i], &url[i..]),
            None => (url, ""),
        };
        let name = percent_decode(name);
        let name = match parse_page_name(&name) {
            Some((stem, _)) => stem.to_string(),
            // Other files (images, attachments) stay relative.
            None if name.contains('.') => return None,
            None => name,
        };
        if name.is_empty() {
            return None;
        }
        Some(self.page_href(&name, fragment))
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
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

/// Render page source to sanitized HTML.
pub fn render(base_url: &str, links: &WikiLinks<'_>, format: &str, text: &str) -> String {
    if format == "markdown" {
        let ctx = RenderContext::new(base_url).with_repo(links.owner, links.repo);
        markdown::render_with_links(text, &ctx, links)
    } else {
        format!("<pre>{}</pre>", escape(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves() {
        let pages = vec![PageFile {
            slug: "Home".into(),
            path: "Home.md".into(),
            format: "markdown",
            sha: String::new(),
        }];
        let l = WikiLinks {
            owner: "o",
            repo: "r",
            pages: &pages,
        };
        assert_eq!(
            l.rewrite_link("home.md#x"),
            Some(("/o/r/wiki/Home#x".into(), "wiki-link"))
        );
        assert_eq!(
            l.rewrite_link("Other%20Page"),
            Some(("/o/r/wiki/Other-Page".into(), "wiki-link wiki-missing"))
        );
        assert_eq!(l.rewrite_link("https://x"), None);
        assert_eq!(l.rewrite_link("img.png"), None);
        assert_eq!(l.rewrite_link("mailto:a@b"), None);
        assert_eq!(
            l.wiki_link("A b?"),
            Some(("/o/r/wiki/A-b".into(), "wiki-link wiki-missing"))
        );
        assert_eq!(render("http://h", &l, "txt", "<b>"), "<pre>&lt;b&gt;</pre>");
    }
}
