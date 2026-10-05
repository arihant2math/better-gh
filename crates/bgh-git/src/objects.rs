//! Owned representations of git objects, parsed from raw object bytes.

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;

use crate::{GitError, GitResult};

/// Author / committer / tagger identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub when: DateTime<Utc>,
    /// Original timezone offset in minutes east of UTC.
    pub offset_minutes: i32,
}

impl Signature {
    /// Parse `Name <email> 1700000000 +0100`.
    pub fn parse(s: &str) -> GitResult<Self> {
        let bad = || GitError::Object(format!("malformed signature: {s:?}"));
        let lt = s.rfind('<').ok_or_else(bad)?;
        let gt = s.rfind('>').ok_or_else(bad)?;
        if gt < lt {
            return Err(bad());
        }
        let name = s[..lt].trim().to_string();
        let email = s[lt + 1..gt].to_string();
        let mut rest = s[gt + 1..].split_whitespace();
        let secs: i64 = rest.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let tz = rest.next().unwrap_or("+0000");
        let offset_minutes = parse_tz(tz).unwrap_or(0);
        let when = Utc.timestamp_opt(secs, 0).single().unwrap_or_default();
        Ok(Self {
            name,
            email,
            when,
            offset_minutes,
        })
    }
}

fn parse_tz(tz: &str) -> Option<i32> {
    let (sign, digits) = match tz.as_bytes().first()? {
        b'+' => (1, &tz[1..]),
        b'-' => (-1, &tz[1..]),
        _ => (1, tz),
    };
    if digits.len() != 4 {
        return None;
    }
    let h: i32 = digits[..2].parse().ok()?;
    let m: i32 = digits[2..].parse().ok()?;
    Some(sign * (h * 60 + m))
}

/// A commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Commit {
    pub sha: String,
    pub tree: String,
    pub parents: Vec<String>,
    pub author: Signature,
    pub committer: Signature,
    pub message: String,
    /// Raw signature block (`gpgsig` header), if signed.
    pub signature: Option<String>,
}

/// Split raw object bytes into headers (with continuation lines joined by
/// `\n`) and the message.
fn split_headers(data: &[u8]) -> (Vec<(String, String)>, String) {
    let text = String::from_utf8_lossy(data);
    let (head, message) = match text.find("\n\n") {
        Some(i) => (&text[..i], text[i + 2..].to_string()),
        None => (text.as_ref(), String::new()),
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        if let Some(cont) = line.strip_prefix(' ') {
            if let Some((_, v)) = headers.last_mut() {
                v.push('\n');
                v.push_str(cont);
            }
        } else if let Some((k, v)) = line.split_once(' ') {
            headers.push((k.to_string(), v.to_string()));
        } else {
            headers.push((line.to_string(), String::new()));
        }
    }
    (headers, message)
}

impl Commit {
    pub fn parse(sha: &str, data: &[u8]) -> GitResult<Self> {
        let (headers, message) = split_headers(data);
        let mut tree = None;
        let mut parents = Vec::new();
        let mut author = None;
        let mut committer = None;
        let mut signature = None;
        for (k, v) in headers {
            match k.as_str() {
                "tree" => tree = Some(v),
                "parent" => parents.push(v),
                "author" => author = Some(Signature::parse(&v)?),
                "committer" => committer = Some(Signature::parse(&v)?),
                "gpgsig" | "gpgsig-sha256" => signature = Some(v),
                _ => {}
            }
        }
        let missing = |f: &str| GitError::Object(format!("commit {sha} has no {f}"));
        let author = author.ok_or_else(|| missing("author"))?;
        Ok(Self {
            sha: sha.to_string(),
            tree: tree.ok_or_else(|| missing("tree"))?,
            parents,
            committer: committer.unwrap_or_else(|| author.clone()),
            author,
            message,
            signature,
        })
    }

    /// First line of the message.
    pub fn summary(&self) -> &str {
        self.message.lines().next().unwrap_or("")
    }
}

/// An annotated tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tag {
    pub sha: String,
    /// Target object SHA.
    pub object: String,
    /// `commit` | `tree` | `blob` | `tag`
    pub object_type: String,
    pub name: String,
    pub tagger: Option<Signature>,
    pub message: String,
    pub signature: Option<String>,
}

impl Tag {
    pub fn parse(sha: &str, data: &[u8]) -> GitResult<Self> {
        let (headers, mut message) = split_headers(data);
        let mut object = None;
        let mut object_type = String::from("commit");
        let mut name = String::new();
        let mut tagger = None;
        for (k, v) in headers {
            match k.as_str() {
                "object" => object = Some(v),
                "type" => object_type = v,
                "tag" => name = v,
                "tagger" => tagger = Signature::parse(&v).ok(),
                _ => {}
            }
        }
        let signature = message.find("-----BEGIN ").map(|i| {
            let sig = message[i..].to_string();
            message.truncate(i);
            sig
        });
        Ok(Self {
            sha: sha.to_string(),
            object: object.ok_or_else(|| GitError::Object(format!("tag {sha} has no object")))?,
            object_type,
            name,
            tagger,
            message,
            signature,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeEntryKind {
    Blob,
    Executable,
    Symlink,
    Tree,
    /// Submodule (gitlink).
    Commit,
}

impl TreeEntryKind {
    /// GitHub `type` field: `blob` | `tree` | `commit`.
    pub fn object_type(self) -> &'static str {
        match self {
            Self::Blob | Self::Executable | Self::Symlink => "blob",
            Self::Tree => "tree",
            Self::Commit => "commit",
        }
    }

    /// Contents API `type`: `file` | `dir` | `symlink` | `submodule`.
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Blob | Self::Executable => "file",
            Self::Symlink => "symlink",
            Self::Tree => "dir",
            Self::Commit => "submodule",
        }
    }
}

/// One entry of a tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeEntry {
    pub name: String,
    /// Six-digit octal mode as GitHub reports it (`100644`, `040000`, ...).
    pub mode: String,
    pub kind: TreeEntryKind,
    pub sha: String,
}

/// Parse a raw tree object.
pub fn parse_tree(data: &[u8]) -> GitResult<Vec<TreeEntry>> {
    let bad = || GitError::Object("malformed tree".into());
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let sp = data[i..].iter().position(|&b| b == b' ').ok_or_else(bad)? + i;
        let nul = data[sp..].iter().position(|&b| b == 0).ok_or_else(bad)? + sp;
        if nul + 21 > data.len() {
            return Err(bad());
        }
        let mode_raw = std::str::from_utf8(&data[i..sp]).map_err(|_| bad())?;
        let name = String::from_utf8_lossy(&data[sp + 1..nul]).into_owned();
        let sha = hex(&data[nul + 1..nul + 21]);
        let mode = format!("{mode_raw:0>6}");
        let kind = match mode.as_str() {
            "040000" => TreeEntryKind::Tree,
            "160000" => TreeEntryKind::Commit,
            "120000" => TreeEntryKind::Symlink,
            "100755" => TreeEntryKind::Executable,
            _ => TreeEntryKind::Blob,
        };
        out.push(TreeEntry {
            name,
            mode,
            kind,
            sha,
        });
        i = nul + 21;
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commit() {
        let raw = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
parent 1111111111111111111111111111111111111111\n\
author A U Thor <a@example.com> 1700000000 +0130\n\
committer C O Mitter <c@example.com> 1700000100 -0500\n\
gpgsig -----BEGIN PGP SIGNATURE-----\n \n abc\n -----END PGP SIGNATURE-----\n\
\n\
Subject line\n\nBody\n";
        let c = Commit::parse("deadbeef", raw).unwrap();
        assert_eq!(c.tree, "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
        assert_eq!(c.parents.len(), 1);
        assert_eq!(c.author.name, "A U Thor");
        assert_eq!(c.author.email, "a@example.com");
        assert_eq!(c.author.offset_minutes, 90);
        assert_eq!(c.committer.offset_minutes, -300);
        assert_eq!(c.author.when.timestamp(), 1_700_000_000);
        assert_eq!(c.summary(), "Subject line");
        assert!(c.signature.unwrap().contains("abc"));
    }

    #[test]
    fn parses_tree() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"100644 README.md\0");
        raw.extend_from_slice(&[0xab; 20]);
        raw.extend_from_slice(b"40000 src\0");
        raw.extend_from_slice(&[0x01; 20]);
        let entries = parse_tree(&raw).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "README.md");
        assert_eq!(entries[0].sha, "ab".repeat(20));
        assert_eq!(entries[1].mode, "040000");
        assert_eq!(entries[1].kind, TreeEntryKind::Tree);
        assert!(parse_tree(b"100644 x\0abc").is_err());
    }

    #[test]
    fn parses_tag() {
        let raw = b"object 1111111111111111111111111111111111111111\ntype commit\ntag v1.0\n\
tagger T <t@example.com> 1700000000 +0000\n\nRelease 1.0\n";
        let t = Tag::parse("x", raw).unwrap();
        assert_eq!(t.name, "v1.0");
        assert_eq!(t.message, "Release 1.0\n");
        assert!(t.tagger.is_some());
    }
}
