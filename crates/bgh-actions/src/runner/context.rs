//! Expression context for steps ([`crate::expr::Context`]) and `hashFiles`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::expr::{Context, ExprError, JobStatus};
use crate::workflow::filter_matches;

pub struct EvalContext {
    /// Lower-cased context name -> value.
    pub contexts: Map<String, Value>,
    pub status: JobStatus,
    /// Host path of GITHUB_WORKSPACE.
    pub host_workspace: PathBuf,
    /// GITHUB_WORKSPACE as the job sees it.
    pub guest_workspace: String,
}

impl Context for EvalContext {
    fn lookup(&self, name: &str) -> Option<Value> {
        self.contexts.get(&name.to_ascii_lowercase()).cloned()
    }

    fn status(&self) -> JobStatus {
        self.status
    }

    fn hash_files(&self, patterns: &[String]) -> Result<String, ExprError> {
        hash_files(&self.host_workspace, &self.guest_workspace, patterns).map_err(|e| {
            ExprError::Function {
                function: "hashFiles".to_string(),
                message: format!("hashFiles failed: {e}"),
            }
        })
    }
}

/// All files under `root`, as `/`-separated paths relative to it, sorted.
pub fn list_files(root: &Path) -> std::io::Result<Vec<String>> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> std::io::Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let ft = e.file_type()?;
            if ft.is_dir() {
                walk(&e.path(), &rel, out)?;
            } else if ft.is_file() || (ft.is_symlink() && e.path().is_file()) {
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    if root.is_dir() {
        walk(root, "", &mut out)?;
    }
    out.sort();
    Ok(out)
}

/// GitHub's `hashFiles`: sha256 over the sha256 digests of every matching
/// file (sorted by path); `''` when nothing matches.
pub fn hash_files(
    host_workspace: &Path,
    guest_workspace: &str,
    patterns: &[String],
) -> std::io::Result<String> {
    let host_ws = host_workspace.to_string_lossy().into_owned();
    let mut pats = Vec::new();
    for p in patterns.iter().flat_map(|p| p.lines()) {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        let (neg, body) = match p.strip_prefix('!') {
            Some(b) => (true, b),
            None => (false, p),
        };
        let mut body = body;
        for prefix in [guest_workspace, host_ws.as_str()] {
            if let Some(rest) = body.strip_prefix(prefix)
                && (rest.is_empty() || rest.starts_with('/'))
            {
                body = rest.trim_start_matches('/');
            }
        }
        let body = body.trim_start_matches("./");
        pats.push(if neg {
            format!("!{body}")
        } else {
            body.to_string()
        });
    }
    let files = list_files(host_workspace)?;
    let mut outer = Sha256::new();
    let mut any = false;
    for f in files {
        if !filter_matches(&pats, &f) {
            continue;
        }
        let data = std::fs::read(host_workspace.join(&f))?;
        outer.update(Sha256::digest(&data));
        any = true;
    }
    Ok(if any {
        hex::encode(outer.finalize())
    } else {
        String::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_files_matches_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::create_dir_all(ws.join("src/sub")).unwrap();
        std::fs::write(ws.join("a.lock"), "A").unwrap();
        std::fs::write(ws.join("src/sub/b.lock"), "B").unwrap();
        std::fs::write(ws.join("src/c.txt"), "C").unwrap();
        let expected = {
            let mut h = Sha256::new();
            h.update(Sha256::digest(b"A"));
            h.update(Sha256::digest(b"B"));
            hex::encode(h.finalize())
        };
        let got = hash_files(ws, "/ws", &["**/*.lock".to_string()]).unwrap();
        assert_eq!(got, expected);
        let got = hash_files(ws, "/ws", &["/ws/**/*.lock".to_string()]).unwrap();
        assert_eq!(got, expected);
        let only_a = hash_files(ws, "/ws", &["**/*.lock\n!src/**".to_string()]).unwrap();
        let mut h = Sha256::new();
        h.update(Sha256::digest(b"A"));
        assert_eq!(only_a, hex::encode(h.finalize()));
        assert_eq!(
            hash_files(ws, "/ws", &["nothing*".to_string()]).unwrap(),
            ""
        );
    }
}
