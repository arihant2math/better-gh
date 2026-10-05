//! A small namespace-aware XML tree and XML canonicalization (C14N 1.0 and
//! exclusive C14N, without comments) for SAML messages.
//!
//! Built on the `xmlparser` tokenizer so prefixes are kept exactly as
//! written (canonicalization renders them). Documents with a DTD are
//! refused (no entity expansion), as are undeclared prefixes, unknown
//! entities, mismatched tags and trees deeper than [`MAX_DEPTH`].

use std::collections::BTreeMap;

pub const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";
const MAX_DEPTH: usize = 64;
const MAX_ELEMENTS: usize = 20_000;

/// An attribute (namespace declarations are kept apart).
#[derive(Debug, Clone)]
pub struct Attr {
    pub prefix: String,
    pub local: String,
    /// Namespace URI ("" for unprefixed attributes).
    pub ns: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub enum Node {
    Element(usize),
    Text(String),
    Comment,
    Pi { target: String, content: String },
}

#[derive(Debug, Clone)]
pub struct Element {
    pub prefix: String,
    pub local: String,
    pub ns: String,
    pub attrs: Vec<Attr>,
    /// Every namespace in scope (prefix → URI; "" is the default
    /// namespace, mapped to "" when undeclared).
    pub inscope: BTreeMap<String, String>,
    pub children: Vec<Node>,
    pub parent: Option<usize>,
}

/// A parsed document: elements in document order, `0` is the root.
#[derive(Debug, Clone)]
pub struct Document {
    pub elements: Vec<Element>,
}

/// Parse errors are plain messages (never shown to end users verbatim).
pub type XmlResult<T> = Result<T, String>;

fn decode(raw: &str, attr: bool) -> XmlResult<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '&' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some(';') => break,
                        Some(c) if name.len() < 16 => name.push(c),
                        _ => return Err("bad character reference".into()),
                    }
                }
                let ch = match name.as_str() {
                    "lt" => '<',
                    "gt" => '>',
                    "amp" => '&',
                    "apos" => '\'',
                    "quot" => '"',
                    n if n.starts_with("#x") => u32::from_str_radix(&n[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or("bad character reference")?,
                    n if n.starts_with('#') => n[1..]
                        .parse::<u32>()
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or("bad character reference")?,
                    _ => return Err(format!("unknown entity &{name};")),
                };
                out.push(ch);
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(if attr { ' ' } else { '\n' });
            }
            '\n' | '\t' if attr => out.push(' '),
            c => out.push(c),
        }
    }
    Ok(out)
}

struct Pending {
    prefix: String,
    local: String,
    attrs: Vec<(String, String, String)>,
}

impl Document {
    pub fn parse(text: &str) -> XmlResult<Document> {
        use xmlparser::{ElementEnd, Token, Tokenizer};
        let mut elements: Vec<Element> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        let mut pending: Option<Pending> = None;
        let mut done = false;
        for token in Tokenizer::from(text) {
            let token = token.map_err(|e| format!("malformed XML: {e}"))?;
            match token {
                Token::Declaration { .. } => {}
                Token::DtdStart { .. }
                | Token::EmptyDtd { .. }
                | Token::EntityDeclaration { .. }
                | Token::DtdEnd { .. } => return Err("DTDs are not allowed".into()),
                Token::ProcessingInstruction {
                    target, content, ..
                } => {
                    if let Some(&cur) = stack.last() {
                        elements[cur].children.push(Node::Pi {
                            target: target.as_str().to_string(),
                            content: content.map(|c| c.as_str().to_string()).unwrap_or_default(),
                        });
                    }
                }
                Token::Comment { .. } => {
                    if let Some(&cur) = stack.last() {
                        elements[cur].children.push(Node::Comment);
                    }
                }
                Token::ElementStart { prefix, local, .. } => {
                    if done {
                        return Err("more than one root element".into());
                    }
                    if stack.len() >= MAX_DEPTH || elements.len() >= MAX_ELEMENTS {
                        return Err("document too large".into());
                    }
                    pending = Some(Pending {
                        prefix: prefix.as_str().to_string(),
                        local: local.as_str().to_string(),
                        attrs: Vec::new(),
                    });
                }
                Token::Attribute {
                    prefix,
                    local,
                    value,
                    ..
                } => {
                    let p = pending.as_mut().ok_or("attribute outside an element")?;
                    p.attrs.push((
                        prefix.as_str().to_string(),
                        local.as_str().to_string(),
                        decode(value.as_str(), true)?,
                    ));
                }
                Token::ElementEnd { end, .. } => match end {
                    ElementEnd::Open | ElementEnd::Empty => {
                        let p = pending.take().ok_or("unexpected element end")?;
                        let parent = stack.last().copied();
                        let mut inscope = match parent {
                            Some(i) => elements[i].inscope.clone(),
                            None => BTreeMap::from([(String::new(), String::new())]),
                        };
                        let mut attrs = Vec::new();
                        for (prefix, local, value) in p.attrs {
                            if prefix.is_empty() && local == "xmlns" {
                                inscope.insert(String::new(), value);
                            } else if prefix == "xmlns" {
                                if value.is_empty() {
                                    return Err("empty namespace URI".into());
                                }
                                inscope.insert(local, value);
                            } else {
                                attrs.push((prefix, local, value));
                            }
                        }
                        let resolve = |prefix: &str| -> XmlResult<String> {
                            if prefix == "xml" {
                                return Ok(NS_XML.into());
                            }
                            inscope
                                .get(prefix)
                                .cloned()
                                .ok_or_else(|| format!("undeclared prefix {prefix:?}"))
                        };
                        let ns = resolve(&p.prefix)?;
                        let mut out_attrs = Vec::new();
                        for (prefix, local, value) in attrs {
                            let ans = if prefix.is_empty() {
                                String::new()
                            } else {
                                resolve(&prefix)?
                            };
                            if out_attrs
                                .iter()
                                .any(|a: &Attr| a.ns == ans && a.local == local)
                            {
                                return Err("duplicate attribute".into());
                            }
                            out_attrs.push(Attr {
                                prefix,
                                local,
                                ns: ans,
                                value,
                            });
                        }
                        let idx = elements.len();
                        elements.push(Element {
                            prefix: p.prefix,
                            local: p.local,
                            ns,
                            attrs: out_attrs,
                            inscope,
                            children: Vec::new(),
                            parent,
                        });
                        if let Some(parent) = parent {
                            elements[parent].children.push(Node::Element(idx));
                        }
                        if matches!(end, ElementEnd::Open) {
                            stack.push(idx);
                        } else if parent.is_none() {
                            done = true;
                        }
                    }
                    ElementEnd::Close(prefix, local) => {
                        let cur = stack.pop().ok_or("unexpected close tag")?;
                        let e = &elements[cur];
                        if e.prefix != prefix.as_str() || e.local != local.as_str() {
                            return Err("mismatched close tag".into());
                        }
                        if stack.is_empty() {
                            done = true;
                        }
                    }
                },
                Token::Text { text } | Token::Cdata { text, .. } => {
                    let decoded = if matches!(token, Token::Cdata { .. }) {
                        text.as_str().replace("\r\n", "\n").replace('\r', "\n")
                    } else {
                        decode(text.as_str(), false)?
                    };
                    match stack.last() {
                        Some(&cur) => {
                            let children = &mut elements[cur].children;
                            if let Some(Node::Text(t)) = children.last_mut() {
                                t.push_str(&decoded);
                            } else {
                                children.push(Node::Text(decoded));
                            }
                        }
                        None if decoded.trim().is_empty() => {}
                        None => return Err("text outside the root element".into()),
                    }
                }
            }
        }
        if !done || !stack.is_empty() {
            return Err("unexpected end of document".into());
        }
        Ok(Document { elements })
    }

    pub fn el(&self, i: usize) -> &Element {
        &self.elements[i]
    }

    /// Child elements of `i`.
    pub fn children(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        self.elements[i].children.iter().filter_map(|n| match n {
            Node::Element(c) => Some(*c),
            _ => None,
        })
    }

    pub fn is(&self, i: usize, ns: &str, local: &str) -> bool {
        let e = &self.elements[i];
        e.ns == ns && e.local == local
    }

    /// Child elements of `i` named `{ns}local`.
    pub fn children_named<'a>(
        &'a self,
        i: usize,
        ns: &'a str,
        local: &'a str,
    ) -> impl Iterator<Item = usize> + 'a {
        self.children(i).filter(move |c| self.is(*c, ns, local))
    }

    pub fn child(&self, i: usize, ns: &str, local: &str) -> Option<usize> {
        self.children_named(i, ns, local).next()
    }

    /// Descendants of `i` (not `i` itself) named `{ns}local`, in document
    /// order.
    pub fn descendants_named(&self, i: usize, ns: &str, local: &str) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<usize> = self.children(i).collect();
        stack.reverse();
        while let Some(c) = stack.pop() {
            if self.is(c, ns, local) {
                out.push(c);
            }
            let mut kids: Vec<usize> = self.children(c).collect();
            kids.reverse();
            stack.extend(kids);
        }
        out
    }

    /// An unqualified attribute.
    pub fn attr(&self, i: usize, local: &str) -> Option<&str> {
        self.elements[i]
            .attrs
            .iter()
            .find(|a| a.ns.is_empty() && a.local == local)
            .map(|a| a.value.as_str())
    }

    /// The concatenated text content of `i` and its descendants.
    pub fn text(&self, i: usize) -> String {
        let mut out = String::new();
        self.collect_text(i, &mut out);
        out
    }

    fn collect_text(&self, i: usize, out: &mut String) {
        for n in &self.elements[i].children {
            match n {
                Node::Text(t) => out.push_str(t),
                Node::Element(c) => self.collect_text(*c, out),
                _ => {}
            }
        }
    }

    /// Elements carrying `ID`, `Id` or `id` = `id` (the reference targets
    /// of XML signatures).
    pub fn elements_with_id(&self, id: &str) -> Vec<usize> {
        (0..self.elements.len())
            .filter(|&i| {
                self.elements[i].attrs.iter().any(|a| {
                    a.ns.is_empty()
                        && matches!(a.local.as_str(), "ID" | "Id" | "id")
                        && a.value == id
                })
            })
            .collect()
    }

    /// Whether `ancestor` is `i` or one of its ancestors.
    pub fn is_within(&self, mut i: usize, ancestor: usize) -> bool {
        loop {
            if i == ancestor {
                return true;
            }
            match self.elements[i].parent {
                Some(p) => i = p,
                None => return false,
            }
        }
    }
}

/// Canonicalization algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum C14n {
    /// Canonical XML 1.0 (also used for 1.1).
    Inclusive,
    /// Exclusive XML canonicalization with an `InclusiveNamespaces`
    /// prefix list (`""` for `#default`).
    Exclusive(Vec<String>),
}

fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

fn qname(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.to_string()
    } else {
        format!("{prefix}:{local}")
    }
}

/// Canonical form of the subtree rooted at `apex` (comments removed, as
/// for a same-document `#id` reference), leaving out the subtree at
/// `exclude` (the enveloped-signature transform).
pub fn canonicalize(doc: &Document, apex: usize, exclude: Option<usize>, mode: &C14n) -> String {
    let mut out = String::new();
    write_element(doc, apex, exclude, mode, &BTreeMap::new(), true, &mut out);
    out
}

fn write_element(
    doc: &Document,
    i: usize,
    exclude: Option<usize>,
    mode: &C14n,
    rendered: &BTreeMap<String, String>,
    apex: bool,
    out: &mut String,
) {
    let e = doc.el(i);
    // Namespace nodes to consider.
    let candidates: Vec<(&String, &String)> = match mode {
        C14n::Inclusive => e.inscope.iter().filter(|(p, _)| *p != "xml").collect(),
        C14n::Exclusive(list) => {
            let mut used: Vec<&str> = vec![e.prefix.as_str()];
            for a in &e.attrs {
                if !a.prefix.is_empty() && a.prefix != "xml" {
                    used.push(&a.prefix);
                }
            }
            e.inscope
                .iter()
                .filter(|(p, _)| {
                    *p != "xml" && (used.contains(&p.as_str()) || list.iter().any(|l| l == *p))
                })
                .collect()
        }
    };
    let mut ns_out: Vec<(&str, &str)> = Vec::new();
    let mut now = rendered.clone();
    for (p, u) in candidates {
        let prev = rendered.get(p).map(String::as_str);
        let render = if p.is_empty() && u.is_empty() {
            prev.is_some_and(|v| !v.is_empty())
        } else {
            prev != Some(u.as_str())
        };
        if render {
            ns_out.push((p, u));
            now.insert(p.clone(), u.clone());
        }
    }
    ns_out.sort();
    let mut attrs: Vec<&Attr> = e.attrs.iter().collect();
    // C14N 1.0: the apex inherits xml:* attributes of its ancestors.
    let mut inherited = Vec::new();
    if apex && *mode == C14n::Inclusive {
        let mut cur = e.parent;
        while let Some(p) = cur {
            for a in &doc.el(p).attrs {
                if a.ns == NS_XML
                    && !attrs.iter().any(|x| x.ns == NS_XML && x.local == a.local)
                    && !inherited.iter().any(|x: &Attr| x.local == a.local)
                {
                    inherited.push(a.clone());
                }
            }
            cur = doc.el(p).parent;
        }
    }
    attrs.extend(inherited.iter());
    attrs.sort_by(|a, b| (a.ns.as_str(), a.local.as_str()).cmp(&(b.ns.as_str(), b.local.as_str())));
    let name = qname(&e.prefix, &e.local);
    out.push('<');
    out.push_str(&name);
    for (p, u) in ns_out {
        if p.is_empty() {
            out.push_str(" xmlns=\"");
        } else {
            out.push_str(" xmlns:");
            out.push_str(p);
            out.push_str("=\"");
        }
        escape_attr(u, out);
        out.push('"');
    }
    for a in attrs {
        out.push(' ');
        out.push_str(&qname(&a.prefix, &a.local));
        out.push_str("=\"");
        escape_attr(&a.value, out);
        out.push('"');
    }
    out.push('>');
    for n in &e.children {
        match n {
            Node::Element(c) if Some(*c) == exclude => {}
            Node::Element(c) => write_element(doc, *c, exclude, mode, &now, false, out),
            Node::Text(t) => escape_text(t, out),
            Node::Comment => {}
            Node::Pi { target, content } => {
                out.push_str("<?");
                out.push_str(target);
                if !content.is_empty() {
                    out.push(' ');
                    out.push_str(content);
                }
                out.push_str("?>");
            }
        }
    }
    out.push_str("</");
    out.push_str(&name);
    out.push('>');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c14n(xml: &str, mode: C14n) -> String {
        let d = Document::parse(xml).unwrap();
        canonicalize(&d, 0, None, &mode)
    }

    #[test]
    fn canonical_basics() {
        let x = "<?xml version=\"1.0\"?>\r\n<a  b='1' a=\"x&amp;y&#9;\"\r\n xmlns=\"urn:d\"><c/><!-- c --><d>t&lt;&#13;\r\n</d></a>";
        assert_eq!(
            c14n(x, C14n::Inclusive),
            "<a xmlns=\"urn:d\" a=\"x&amp;y&#x9;\" b=\"1\"><c></c><d>t&lt;&#xD;\n</d></a>"
        );
    }

    #[test]
    fn exclusive_renders_only_used_namespaces() {
        let x = r#"<r:root xmlns:r="urn:r" xmlns:u="urn:u" xmlns:s="urn:s"><s:a u:x="1"><s:b/></s:a></r:root>"#;
        let d = Document::parse(x).unwrap();
        let a = d.children(0).next().unwrap();
        assert_eq!(
            canonicalize(&d, a, None, &C14n::Exclusive(vec![])),
            r#"<s:a xmlns:s="urn:s" xmlns:u="urn:u" u:x="1"><s:b></s:b></s:a>"#
        );
        assert_eq!(
            canonicalize(&d, a, None, &C14n::Exclusive(vec!["r".into()])),
            r#"<s:a xmlns:r="urn:r" xmlns:s="urn:s" xmlns:u="urn:u" u:x="1"><s:b></s:b></s:a>"#
        );
        assert_eq!(
            canonicalize(&d, a, None, &C14n::Inclusive),
            r#"<s:a xmlns:r="urn:r" xmlns:s="urn:s" xmlns:u="urn:u" u:x="1"><s:b></s:b></s:a>"#
        );
    }

    #[test]
    fn default_namespace_undeclaration() {
        let x = r#"<a xmlns="urn:a"><b xmlns=""><c/></b></a>"#;
        assert_eq!(
            c14n(x, C14n::Exclusive(vec![])),
            r#"<a xmlns="urn:a"><b xmlns=""><c></c></b></a>"#
        );
    }

    #[test]
    fn refuses_dtds_and_bad_input() {
        assert!(Document::parse("<!DOCTYPE a [<!ENTITY x \"y\">]><a>&x;</a>").is_err());
        assert!(Document::parse("<a>&x;</a>").is_err());
        assert!(Document::parse("<p:a/>").is_err());
        assert!(Document::parse("<a></b>").is_err());
        assert!(Document::parse("<a/><b/>").is_err());
        assert!(Document::parse("<a>").is_err());
    }
}
