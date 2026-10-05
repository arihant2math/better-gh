//! Wiki page model: files at the tree root of the wiki repository.
//!
//! `My-Page.md` → slug `My-Page`, title `My Page`, format `markdown`.

use bgh_core::error::{ApiError, ApiResult, FieldError};
use bgh_git::{GitRepo, GitResult, TreeEntry, TreeEntryKind};

/// Branch new wikis are created with.
pub const DEFAULT_BRANCH: &str = "master";
pub const HOME: &str = "Home";
pub const SIDEBAR: &str = "_Sidebar";
pub const FOOTER: &str = "_Footer";

/// Page file extensions and their format names (first match wins when
/// several files share a slug).
const PAGE_EXTS: &[(&str, &str)] = &[
    ("md", "markdown"),
    ("markdown", "markdown"),
    ("mkd", "markdown"),
    ("mkdn", "markdown"),
    ("mdown", "markdown"),
    ("textile", "textile"),
    ("rdoc", "rdoc"),
    ("org", "org"),
    ("creole", "creole"),
    ("rst", "rest"),
    ("rest", "rest"),
    ("asciidoc", "asciidoc"),
    ("adoc", "asciidoc"),
    ("asc", "asciidoc"),
    ("pod", "pod"),
    ("mediawiki", "mediawiki"),
    ("wiki", "mediawiki"),
    ("txt", "txt"),
];

/// A page file in a tree.
#[derive(Debug, Clone)]
pub struct PageFile {
    pub slug: String,
    /// File name at the tree root, e.g. `Home.md`.
    pub path: String,
    pub format: &'static str,
    /// Blob SHA.
    pub sha: String,
}

impl PageFile {
    pub fn title(&self) -> String {
        title_of(&self.slug)
    }

    pub fn is_special(&self) -> bool {
        is_special(&self.slug)
    }

    pub fn ext(&self) -> &str {
        self.path.rsplit_once('.').map(|(_, e)| e).unwrap_or("md")
    }
}

pub fn is_special(slug: &str) -> bool {
    slug == SIDEBAR || slug == FOOTER
}

/// `My-Page` → `My Page`
pub fn title_of(slug: &str) -> String {
    slug.replace('-', " ")
}

/// Characters dropped from slugs (unsafe in file names / URLs).
const UNSAFE: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|', '#', '%'];

/// `My Page?` → `My-Page`: unsafe characters dropped, whitespace runs → `-`
/// (same rule as the web client's `wikiSlug`).
pub fn slug_of(title: &str) -> String {
    title
        .replace(UNSAFE, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

/// Split a file name into `(slug, format)` if it is a page.
pub fn parse_page_name(name: &str) -> Option<(&str, &'static str)> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || stem.starts_with('.') {
        return None;
    }
    let ext = ext.to_ascii_lowercase();
    PAGE_EXTS
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, f)| (stem, *f))
}

fn ext_rank(path: &str) -> usize {
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    PAGE_EXTS
        .iter()
        .position(|(e, _)| *e == ext)
        .unwrap_or(usize::MAX)
}

/// Pages among root tree entries, one per slug, sorted by title.
pub fn pages_from_tree(entries: &[TreeEntry]) -> Vec<PageFile> {
    let mut pages: Vec<PageFile> = entries
        .iter()
        .filter(|e| matches!(e.kind, TreeEntryKind::Blob | TreeEntryKind::Executable))
        .filter_map(|e| {
            let (slug, format) = parse_page_name(&e.name)?;
            Some(PageFile {
                slug: slug.to_string(),
                path: e.name.clone(),
                format,
                sha: e.sha.clone(),
            })
        })
        .collect();
    pages.sort_by(|a, b| {
        a.slug
            .to_lowercase()
            .cmp(&b.slug.to_lowercase())
            .then_with(|| a.slug.cmp(&b.slug))
            .then_with(|| ext_rank(&a.path).cmp(&ext_rank(&b.path)))
    });
    pages.dedup_by(|b, a| a.slug == b.slug);
    pages
}

/// Find a page by slug: exact match first, then case-insensitive.
pub fn find<'a>(pages: &'a [PageFile], slug: &str) -> Option<&'a PageFile> {
    pages.iter().find(|p| p.slug == slug).or_else(|| {
        let lower = slug.to_lowercase();
        pages.iter().find(|p| p.slug.to_lowercase() == lower)
    })
}

/// A tree snapshot of the wiki at one commit.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub commit: String,
    pub pages: Vec<PageFile>,
}

impl Snapshot {
    pub fn find(&self, slug: &str) -> Option<&PageFile> {
        find(&self.pages, slug)
    }

    /// Regular (non-special) pages.
    pub fn listed(&self) -> impl Iterator<Item = &PageFile> {
        self.pages.iter().filter(|p| !p.is_special())
    }
}

/// The commit HEAD points to, `None` for an empty wiki.
pub fn head_commit(r: &GitRepo) -> GitResult<Option<String>> {
    if r.resolve("HEAD")?.is_none() {
        return Ok(None);
    }
    r.resolve_commit("HEAD").map(Some)
}

/// Read the root tree of `commit`.
pub fn snapshot_at(r: &GitRepo, commit: &str) -> GitResult<Snapshot> {
    let c = r.commit(commit)?;
    let entries = r.tree(&c.tree)?;
    Ok(Snapshot {
        commit: commit.to_string(),
        pages: pages_from_tree(&entries),
    })
}

/// Snapshot at `rev` (a SHA or ref) or HEAD. `Ok(None)`: empty wiki.
pub fn snapshot(r: &GitRepo, rev: Option<&str>) -> GitResult<Option<Snapshot>> {
    let commit = match rev.filter(|s| !s.is_empty()) {
        Some(rev) => Some(r.resolve_commit(rev)?),
        None => head_commit(r)?,
    };
    commit.map(|c| snapshot_at(r, &c)).transpose()
}

/// Validate a page title; returns its slug.
pub fn validate_title(title: &str) -> ApiResult<String> {
    let t = title.trim();
    let err = |msg: &str| ApiError::invalid_field(FieldError::custom("WikiPage", "title", msg));
    if t.is_empty() {
        return Err(ApiError::invalid_field(FieldError::missing_field(
            "WikiPage", "title",
        )));
    }
    if t.chars().count() > 255 {
        return Err(err("title is too long (maximum is 255 characters)"));
    }
    if t.contains('/') {
        return Err(err("title cannot contain slashes"));
    }
    if t.chars().any(char::is_control) {
        return Err(err("title contains invalid characters"));
    }
    let slug = slug_of(t);
    if slug.is_empty() {
        return Err(err("title must contain at least one valid character"));
    }
    if slug.starts_with('.') {
        return Err(err("title cannot start with a period"));
    }
    Ok(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> TreeEntry {
        TreeEntry {
            name: name.into(),
            mode: "100644".into(),
            kind: TreeEntryKind::Blob,
            sha: "a".repeat(40),
        }
    }

    #[test]
    fn pages_and_titles() {
        let pages = pages_from_tree(&[
            entry("Home.md"),
            entry("logo.png"),
            entry("My-Page.textile"),
            entry("_Sidebar.md"),
            entry("Home.txt"),
            entry(".hidden.md"),
        ]);
        let slugs: Vec<_> = pages.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(slugs, ["_Sidebar.md", "Home.md", "My-Page.textile"]);
        assert_eq!(pages[2].title(), "My Page");
        assert_eq!(pages[2].format, "textile");
        assert!(pages[0].is_special());
        assert_eq!(find(&pages, "my-page").unwrap().slug, "My-Page");
        assert_eq!(slug_of(" A  b? "), "A-b");
        assert_eq!(slug_of("What: is #1 <x>"), "What-is-1-x");
        assert!(validate_title("???").is_err());
        assert!(validate_title("a/b").is_err());
        assert!(validate_title("  ").is_err());
        assert!(validate_title(&"x".repeat(256)).is_err());
        assert_eq!(validate_title("Hello World").unwrap(), "Hello-World");
    }
}
