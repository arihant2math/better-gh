//! OCI content digests (`sha256:<hex>`, `sha512:<hex>`).

use sha2::{Digest as _, Sha256, Sha512};

/// Digest algorithms the registry accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Algorithm {
    Sha256,
    Sha512,
}

impl Algorithm {
    pub fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    fn hex_len(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }

    pub fn hasher(self) -> Hasher {
        match self {
            Self::Sha256 => Hasher::Sha256(Sha256::new()),
            Self::Sha512 => Hasher::Sha512(Box::new(Sha512::new())),
        }
    }
}

/// A validated digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Digest {
    pub algorithm: Algorithm,
    /// Lowercase hex.
    pub hex: String,
}

impl Digest {
    /// Parse `alg:hex`; `None` for unsupported algorithms or malformed hex.
    pub fn parse(s: &str) -> Option<Self> {
        let (alg, hex) = s.split_once(':')?;
        let algorithm = match alg {
            "sha256" => Algorithm::Sha256,
            "sha512" => Algorithm::Sha512,
            _ => return None,
        };
        let valid = hex.len() == algorithm.hex_len()
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        valid.then(|| Self {
            algorithm,
            hex: hex.to_string(),
        })
    }

    /// Whether `s` looks like a digest at all (`alg:encoded`), supported or
    /// not: manifests references containing `:` are digests, never tags.
    pub fn looks_like(s: &str) -> bool {
        s.contains(':')
    }

    pub fn of(algorithm: Algorithm, data: &[u8]) -> Self {
        let mut h = algorithm.hasher();
        h.update(data);
        h.finish()
    }

    pub fn sha256(data: &[u8]) -> Self {
        Self::of(Algorithm::Sha256, data)
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.algorithm.name(), self.hex)
    }
}

/// Incremental hasher for either algorithm.
pub enum Hasher {
    Sha256(Sha256),
    Sha512(Box<Sha512>),
}

impl Hasher {
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Self::Sha256(h) => h.update(data),
            Self::Sha512(h) => h.update(data),
        }
    }

    pub fn finish(self) -> Digest {
        match self {
            Self::Sha256(h) => Digest {
                algorithm: Algorithm::Sha256,
                hex: hex::encode(h.finalize()),
            },
            Self::Sha512(h) => Digest {
                algorithm: Algorithm::Sha512,
                hex: hex::encode(h.finalize()),
            },
        }
    }
}

/// OCI repository name component rules (lowercase, separators `.` `_`
/// `__` `-`, path segments separated by `/`).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.split('/').all(|seg| {
            let b = seg.as_bytes();
            if b.is_empty() || !is_alnum(b[0]) || !is_alnum(b[b.len() - 1]) {
                return false;
            }
            let mut i = 0;
            while i < b.len() {
                if is_alnum(b[i]) {
                    i += 1;
                    continue;
                }
                // Separator run: `.`, `_`, `__` or one or more `-`.
                let start = i;
                while i < b.len() && !is_alnum(b[i]) {
                    i += 1;
                }
                let sep = &seg[start..i];
                if !(sep == "." || sep == "_" || sep == "__" || sep.bytes().all(|c| c == b'-')) {
                    return false;
                }
            }
            true
        })
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// Tag rules: `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`.
pub fn valid_tag(tag: &str) -> bool {
    let b = tag.as_bytes();
    !b.is_empty()
        && b.len() <= 128
        && (b[0].is_ascii_alphanumeric() || b[0] == b'_')
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests() {
        let d = Digest::sha256(b"");
        assert_eq!(
            d.to_string(),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(Digest::parse(&d.to_string()), Some(d));
        assert!(Digest::parse("sha256:abc").is_none());
        assert!(Digest::parse("md5:d41d8cd98f00b204e9800998ecf8427e").is_none());
        assert!(
            Digest::parse(
                "sha256:E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"
            )
            .is_none()
        );
        let d = Digest::of(Algorithm::Sha512, b"x");
        assert_eq!(Digest::parse(&d.to_string()), Some(d));
    }

    #[test]
    fn names_and_tags() {
        for ok in ["a", "app", "my-app/sub_dir", "a__b", "a.b-c---d", "x/y/z1"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "", "A", "a/", "/a", "a//b", "-a", "a-", "a___b", "a._b", "a b",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(valid_tag("latest"));
        assert!(valid_tag("_v1.0-rc"));
        assert!(!valid_tag(".x"));
        assert!(!valid_tag(&"a".repeat(129)));
    }
}
