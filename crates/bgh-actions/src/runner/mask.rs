//! Secret masking: every registered value is replaced by `***` in logs.

use std::sync::RwLock;

#[derive(Debug, Default)]
pub struct Masker {
    /// Sorted by length, longest first, so overlapping secrets mask fully.
    values: RwLock<Vec<String>>,
}

impl Masker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a secret. Multi-line values also register each non-empty
    /// line on its own (like GitHub).
    pub fn add(&self, value: &str) {
        let mut candidates = Vec::new();
        let trimmed = value.trim_end_matches(['\r', '\n']);
        if !trimmed.trim().is_empty() {
            candidates.push(trimmed.to_string());
        }
        if trimmed.contains('\n') {
            for line in trimmed.lines() {
                let line = line.trim_end_matches('\r');
                if !line.trim().is_empty() {
                    candidates.push(line.to_string());
                }
            }
        }
        if candidates.is_empty() {
            return;
        }
        let mut values = self.values.write().unwrap_or_else(|e| e.into_inner());
        for c in candidates {
            if !values.contains(&c) {
                values.push(c);
            }
        }
        values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    }

    pub fn mask(&self, text: &str) -> String {
        let values = self.values.read().unwrap_or_else(|e| e.into_inner());
        let mut out = text.to_string();
        for v in values.iter() {
            if out.contains(v.as_str()) {
                out = out.replace(v.as_str(), "***");
            }
        }
        out
    }

    /// Whether `text` contains any registered secret.
    pub fn contains_secret(&self, text: &str) -> bool {
        let values = self.values.read().unwrap_or_else(|e| e.into_inner());
        values.iter().any(|v| text.contains(v.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_longest_first_and_lines() {
        let m = Masker::new();
        m.add("abc");
        m.add("abcdef");
        m.add("line1\nline2\n");
        m.add("");
        assert_eq!(m.mask("x abcdef y abc"), "x *** y ***");
        assert_eq!(m.mask("line2 and line1"), "*** and ***");
        assert!(m.contains_secret("zzline1"));
        assert!(!m.contains_secret("nothing"));
    }
}
