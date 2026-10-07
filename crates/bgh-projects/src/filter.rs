//! The small filter language of the auto-add workflow
//! (`is:issue,pr is:open label:bug -label:wontfix "some words"`).

/// What a filter is evaluated against.
pub struct Candidate<'a> {
    pub is_pr: bool,
    pub open: bool,
    pub title: &'a str,
    pub labels: &'a [String],
}

/// Split on whitespace, keeping `"quoted strings"` (also after `key:`) together.
pub(crate) fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Whether `c` matches `filter`. Qualifiers are ANDed; comma-separated
/// values within one qualifier are ORed; a leading `-` negates. Unknown
/// qualifiers are ignored.
pub fn matches(filter: &str, c: &Candidate<'_>) -> bool {
    let title = c.title.to_lowercase();
    tokens(filter).iter().all(|tok| {
        let (neg, tok) = match tok.strip_prefix('-') {
            Some(t) => (true, t),
            None => (false, tok.as_str()),
        };
        let hit = match tok.split_once(':') {
            Some((key, vals)) => {
                let vals: Vec<String> = vals
                    .split(',')
                    .map(|v| v.trim().to_lowercase())
                    .filter(|v| !v.is_empty())
                    .collect();
                match key.to_lowercase().as_str() {
                    "is" => vals.iter().any(|v| match v.as_str() {
                        "issue" => !c.is_pr,
                        "pr" => c.is_pr,
                        "open" => c.open,
                        "closed" => !c.open,
                        _ => true,
                    }),
                    "label" => vals
                        .iter()
                        .any(|v| c.labels.iter().any(|l| l.to_lowercase() == *v)),
                    "no" if vals.iter().any(|v| v == "label") => c.labels.is_empty(),
                    _ => true,
                }
            }
            None => title.contains(&tok.to_lowercase()),
        };
        hit != neg
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates() {
        let labels = vec!["bug".to_string(), "good first issue".to_string()];
        let c = Candidate {
            is_pr: false,
            open: true,
            title: "Crash on start",
            labels: &labels,
        };
        assert!(matches("", &c));
        assert!(matches("is:issue,pr is:open", &c));
        assert!(!matches("is:pr", &c));
        assert!(matches("label:bug", &c));
        assert!(matches("label:\"good first issue\"", &c));
        assert!(!matches("-label:bug", &c));
        assert!(matches("label:docs,bug crash", &c));
        assert!(!matches("is:closed", &c));
        assert!(!matches("no:label", &c));
    }
}
