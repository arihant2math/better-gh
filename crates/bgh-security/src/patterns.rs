//! The secret pattern engine: built-in provider patterns (high
//! confidence, push protected), non-provider patterns (generic, opt-in per
//! repository) and custom patterns (per repository or organization).
//!
//! All regexes use the `regex` crate (linear time, no backtracking), so a
//! custom pattern can't stall a scan. Each blob is matched once against a
//! [`regex::bytes::RegexSet`]; only the patterns that hit run again to
//! extract their matches.

use std::sync::{Arc, OnceLock};

use regex::bytes::{Regex, RegexBuilder, RegexSet, RegexSetBuilder};
use sha2::{Digest, Sha256};

/// Where a pattern comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// High-confidence token formats of known providers.
    Provider,
    /// Generic formats (connection strings, auth headers); repositories
    /// opt in with `secret_scanning_non_provider_patterns`.
    NonProvider,
    /// A repository or organization custom pattern (row id).
    Custom(i64),
}

#[derive(Debug, Clone)]
pub struct Pattern {
    /// GitHub's `secret_type` (`aws_access_key_id`, `custom_pattern_12`).
    pub secret_type: String,
    pub display_name: String,
    pub regex: Regex,
    pub kind: Kind,
    /// Blocks pushes when push protection is on.
    pub push_protected: bool,
}

/// A built-in pattern: `(secret_type, display name, regex, kind)`. A
/// `secret` capture group, when present, is the secret; otherwise the
/// whole match is.
type Builtin = (&'static str, &'static str, &'static str, Kind);

const BUILTIN: &[Builtin] = &[
    // This server's own credentials (bgh_core::crypto prefixes).
    (
        "bgh_personal_access_token",
        "Better GitHub Personal Access Token",
        r"\bbghp_[A-Za-z0-9]{40}\b",
        Kind::Provider,
    ),
    (
        "bgh_oauth_access_token",
        "Better GitHub OAuth Access Token",
        r"\bbgho_[A-Za-z0-9]{40}\b",
        Kind::Provider,
    ),
    (
        "bgh_app_installation_access_token",
        "Better GitHub App Installation Access Token",
        r"\bbghs_[A-Za-z0-9]{40}\b",
        Kind::Provider,
    ),
    // GitHub.
    (
        "github_personal_access_token",
        "GitHub Personal Access Token",
        r"\bghp_[A-Za-z0-9]{36}\b",
        Kind::Provider,
    ),
    (
        "github_fine_grained_personal_access_token",
        "GitHub Fine-Grained Personal Access Token",
        r"\bgithub_pat_[A-Za-z0-9]{22}_[A-Za-z0-9]{59}\b",
        Kind::Provider,
    ),
    (
        "github_oauth_access_token",
        "GitHub OAuth Access Token",
        r"\bgho_[A-Za-z0-9]{36}\b",
        Kind::Provider,
    ),
    (
        "github_app_installation_access_token",
        "GitHub App Installation Access Token",
        r"\bghs_[A-Za-z0-9]{36}\b",
        Kind::Provider,
    ),
    (
        "github_refresh_token",
        "GitHub Refresh Token",
        r"\bghr_[A-Za-z0-9]{36,76}\b",
        Kind::Provider,
    ),
    // AWS.
    (
        "aws_access_key_id",
        "Amazon AWS Access Key ID",
        r"\b(?P<secret>(?:AKIA|ASIA|ABIA|ACCA)[A-Z2-7]{16})\b",
        Kind::Provider,
    ),
    (
        "aws_secret_access_key",
        "Amazon AWS Secret Access Key",
        r#"(?i:aws.{0,20}?(?:secret|private).{0,20}?)['"]?\s*[:=]\s*['"]?(?P<secret>[A-Za-z0-9/+]{40})\b"#,
        Kind::Provider,
    ),
    // Google.
    (
        "google_api_key",
        "Google API Key",
        r"\bAIza[0-9A-Za-z_\-]{35}\b",
        Kind::Provider,
    ),
    (
        "google_oauth_client_secret",
        "Google OAuth Client Secret",
        r"\bGOCSPX-[A-Za-z0-9_\-]{28}\b",
        Kind::Provider,
    ),
    (
        "google_cloud_private_key_id",
        "Google Cloud Service Account Key ID",
        r#""private_key_id"\s*:\s*"(?P<secret>[a-f0-9]{40})""#,
        Kind::Provider,
    ),
    // Azure.
    (
        "azure_storage_account_key",
        "Azure Storage Account Access Key",
        r"AccountKey=(?P<secret>[A-Za-z0-9+/]{86}==)",
        Kind::Provider,
    ),
    (
        "azure_active_directory_application_secret",
        "Azure Active Directory Application Secret",
        r"\b(?P<secret>[A-Za-z0-9_~.]{3}\dQ~[A-Za-z0-9_~.\-]{31,34})(?:[^A-Za-z0-9_~.\-]|$)",
        Kind::Provider,
    ),
    // Slack.
    (
        "slack_api_token",
        "Slack API Token",
        r"\bxox[abposr]-[0-9]{8,14}-[0-9A-Za-z\-]{10,72}\b",
        Kind::Provider,
    ),
    (
        "slack_incoming_webhook_url",
        "Slack Incoming Webhook URL",
        r"https://hooks\.slack\.com/services/T[A-Z0-9]{8,12}/B[A-Z0-9]{8,12}/[A-Za-z0-9]{24}",
        Kind::Provider,
    ),
    // Stripe.
    (
        "stripe_api_key",
        "Stripe API Key",
        r"\bsk_live_[0-9A-Za-z]{24,99}\b",
        Kind::Provider,
    ),
    (
        "stripe_live_restricted_key",
        "Stripe Live Restricted Key",
        r"\brk_live_[0-9A-Za-z]{24,99}\b",
        Kind::Provider,
    ),
    (
        "stripe_test_secret_key",
        "Stripe Test API Secret Key",
        r"\bsk_test_[0-9A-Za-z]{24,99}\b",
        Kind::Provider,
    ),
    // Private keys (the whole PEM block is the secret).
    (
        "private_key",
        "Private Key",
        r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED |PGP )?PRIVATE KEY(?: BLOCK)?-----[A-Za-z0-9+/=\s:,\-]{32,}?-----END (?:RSA |EC |DSA |OPENSSH |ENCRYPTED |PGP )?PRIVATE KEY(?: BLOCK)?-----",
        Kind::Provider,
    ),
    // Package registries.
    (
        "npm_access_token",
        "npm Access Token",
        r"\bnpm_[A-Za-z0-9]{36}\b",
        Kind::Provider,
    ),
    (
        "pypi_api_token",
        "PyPI API Token",
        r"\bpypi-AgEIcHlwaS5vcmc[A-Za-z0-9_\-]{50,}",
        Kind::Provider,
    ),
    (
        "rubygems_api_key",
        "RubyGems API Key",
        r"\brubygems_[a-f0-9]{48}\b",
        Kind::Provider,
    ),
    (
        "docker_personal_access_token",
        "Docker Personal Access Token",
        r"\bdckr_pat_[A-Za-z0-9_\-]{27}\b",
        Kind::Provider,
    ),
    // Other providers.
    (
        "gitlab_access_token",
        "GitLab Access Token",
        r"\bglpat-[0-9A-Za-z_\-]{20}\b",
        Kind::Provider,
    ),
    (
        "sendgrid_api_key",
        "SendGrid API Key",
        r"\bSG\.[A-Za-z0-9_\-]{22}\.[A-Za-z0-9_\-]{43}\b",
        Kind::Provider,
    ),
    (
        "shopify_access_token",
        "Shopify Access Token",
        r"\bshpat_[a-fA-F0-9]{32}\b",
        Kind::Provider,
    ),
    (
        "openai_api_key",
        "OpenAI API Key",
        r"\bsk-(?:proj-)?[A-Za-z0-9_\-]{20,74}T3BlbkFJ[A-Za-z0-9_\-]{20,74}\b",
        Kind::Provider,
    ),
    (
        "anthropic_api_key",
        "Anthropic API Key",
        r"\bsk-ant-api03-[A-Za-z0-9_\-]{93}AA\b",
        Kind::Provider,
    ),
    (
        "hashicorp_vault_service_token",
        "HashiCorp Vault Service Token",
        r"\bhvs\.[A-Za-z0-9_\-]{90,120}\b",
        Kind::Provider,
    ),
    // Non-provider (generic) patterns.
    (
        "http_basic_authentication_header",
        "HTTP Basic Authentication Header",
        r"(?i:authorization:\s*basic\s+)(?P<secret>[A-Za-z0-9+/]{8,}={0,2})",
        Kind::NonProvider,
    ),
    (
        "http_bearer_authentication_header",
        "HTTP Bearer Authentication Header",
        r"(?i:authorization:\s*bearer\s+)(?P<secret>[A-Za-z0-9_\-.=+/]{16,})",
        Kind::NonProvider,
    ),
    (
        "postgres_connection_string",
        "PostgreSQL Connection String",
        r"\bpostgres(?:ql)?://[^:@\s/'\x22]+:(?P<secret>[^@\s/'\x22]{3,})@[^\s'\x22]+",
        Kind::NonProvider,
    ),
    (
        "mysql_connection_string",
        "MySQL Connection String",
        r"\bmysql://[^:@\s/'\x22]+:(?P<secret>[^@\s/'\x22]{3,})@[^\s'\x22]+",
        Kind::NonProvider,
    ),
    (
        "mongodb_connection_string",
        "MongoDB Connection String",
        r"\bmongodb(?:\+srv)?://[^:@\s/'\x22]+:(?P<secret>[^@\s/'\x22]{3,})@[^\s'\x22]+",
        Kind::NonProvider,
    ),
];

/// Largest custom pattern accepted (characters).
pub const MAX_CUSTOM_PATTERN_LEN: usize = 1000;

/// Compiled-size limit of one custom regex.
const CUSTOM_SIZE_LIMIT: usize = 1 << 20;

/// Compile a custom pattern (or explain why it's invalid).
pub fn compile_custom(pattern: &str) -> Result<Regex, String> {
    if pattern.trim().is_empty() {
        return Err("pattern is empty".into());
    }
    if pattern.chars().count() > MAX_CUSTOM_PATTERN_LEN {
        return Err(format!(
            "pattern is longer than {MAX_CUSTOM_PATTERN_LEN} characters"
        ));
    }
    let re = RegexBuilder::new(pattern)
        .unicode(false)
        .size_limit(CUSTOM_SIZE_LIMIT)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => "pattern is too complex".to_string(),
            other => other.to_string(),
        })?;
    if re.is_match(b"") {
        return Err("pattern matches the empty string".into());
    }
    Ok(re)
}

/// A secret found in a blob. Lines and columns are 1-based; `end_column`
/// is the column just past the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Index into [`Engine::patterns`].
    pub pattern: usize,
    pub secret: String,
    pub start_line: i32,
    pub end_line: i32,
    pub start_column: i32,
    pub end_column: i32,
}

/// SHA-256 of a secret (alert dedupe key).
pub fn secret_hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

/// Documentation placeholders that are never reported.
fn is_placeholder(secret: &str) -> bool {
    secret.contains("EXAMPLE")
        || secret.contains("XXXXXXXX")
        || secret.contains("xxxxxxxx")
        || secret.contains("00000000000000")
}

#[derive(Debug, Clone)]
pub struct Engine {
    pub patterns: Vec<Pattern>,
    set: RegexSet,
}

impl Engine {
    /// The built-in provider patterns (compiled once).
    pub fn builtin() -> Arc<Engine> {
        static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();
        ENGINE
            .get_or_init(|| {
                Arc::new(Engine::new(
                    builtin_patterns()
                        .into_iter()
                        .filter(|p| p.kind == Kind::Provider)
                        .collect(),
                ))
            })
            .clone()
    }

    pub fn new(patterns: Vec<Pattern>) -> Self {
        let set = RegexSetBuilder::new(patterns.iter().map(|p| p.regex.as_str()))
            .unicode(false)
            .size_limit(64 << 20)
            .dfa_size_limit(64 << 20)
            .build()
            .unwrap_or_else(|_| RegexSet::empty());
        Self { patterns, set }
    }

    /// The patterns that apply to a repository: provider patterns, the
    /// non-provider ones when enabled, and `custom` ones.
    pub fn for_repo(non_provider: bool, custom: Vec<Pattern>) -> Arc<Engine> {
        if !non_provider && custom.is_empty() {
            return Self::builtin();
        }
        let mut patterns: Vec<Pattern> = builtin_patterns()
            .into_iter()
            .filter(|p| non_provider || p.kind != Kind::NonProvider)
            .collect();
        patterns.extend(custom);
        Arc::new(Engine::new(patterns))
    }

    /// Only the push-protected patterns.
    pub fn push_protected(&self) -> Arc<Engine> {
        Arc::new(Engine::new(
            self.patterns
                .iter()
                .filter(|p| p.push_protected)
                .cloned()
                .collect(),
        ))
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Every secret in `data` (text only: blobs with a NUL byte in their
    /// first 8 KB are skipped, like git's binary detection).
    pub fn scan(&self, data: &[u8]) -> Vec<Finding> {
        if self.patterns.is_empty() || is_binary(data) {
            return vec![];
        }
        let hits = self.set.matches(data);
        if !hits.matched_any() {
            return vec![];
        }
        let mut out = Vec::new();
        for i in hits.iter() {
            let p = &self.patterns[i];
            let group = p.regex.capture_names().position(|n| n == Some("secret"));
            for caps in p.regex.captures_iter(data) {
                let m = match group.and_then(|g| caps.get(g)) {
                    Some(m) => m,
                    None => caps.get(0).expect("group 0"),
                };
                let Ok(secret) = std::str::from_utf8(m.as_bytes()) else {
                    continue;
                };
                if secret.is_empty() || is_placeholder(secret) {
                    continue;
                }
                let (start_line, start_column) = line_col(data, m.start());
                let (end_line, end_column) = line_col(data, m.end());
                out.push(Finding {
                    pattern: i,
                    secret: secret.to_string(),
                    start_line,
                    end_line,
                    start_column,
                    end_column,
                });
            }
        }
        out
    }
}

fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(8000)].contains(&0)
}

/// 1-based (line, column) of byte offset `at`.
fn line_col(data: &[u8], at: usize) -> (i32, i32) {
    let before = &data[..at];
    let line = before.iter().filter(|b| **b == b'\n').count() + 1;
    let col = at
        - before
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |p| p + 1)
        + 1;
    (line as i32, col as i32)
}

fn builtin_patterns() -> Vec<Pattern> {
    BUILTIN
        .iter()
        .map(|(ty, name, re, kind)| Pattern {
            secret_type: ty.to_string(),
            display_name: name.to_string(),
            regex: RegexBuilder::new(re)
                .unicode(false)
                .build()
                .expect("built-in pattern compiles"),
            kind: *kind,
            push_protected: *kind == Kind::Provider,
        })
        .collect()
}

/// `(secret_type, display name, provider, push protected)` of every
/// built-in pattern.
pub fn builtin_list() -> Vec<(&'static str, &'static str, bool, bool)> {
    BUILTIN
        .iter()
        .map(|(ty, name, _, kind)| (*ty, *name, *kind == Kind::Provider, *kind == Kind::Provider))
        .collect()
}

/// `secret_type` of custom pattern `id`.
pub fn custom_secret_type(id: i64) -> String {
    format!("custom_pattern_{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(e: &Engine, text: &str) -> Vec<String> {
        e.scan(text.as_bytes())
            .into_iter()
            .map(|f| e.patterns[f.pattern].secret_type.clone())
            .collect()
    }

    #[test]
    fn finds_provider_secrets() {
        let e = Engine::builtin();
        let text = "\
aws_access_key_id = AKIAQ3EGRXPZ7K2LMN4D
aws_secret_access_key = \"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\"
token: ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789
slack = xoxb-123456789012-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx
stripe = sk_live_51HxYzAbCdEfGhIjKlMnOpQrStUv
";
        let found = types(&e, text);
        for t in [
            "aws_access_key_id",
            "aws_secret_access_key",
            "github_personal_access_token",
            "slack_api_token",
            "stripe_api_key",
        ] {
            assert!(found.contains(&t.to_string()), "{t} not in {found:?}");
        }
        let f = e.scan(text.as_bytes());
        let aws = f
            .iter()
            .find(|f| e.patterns[f.pattern].secret_type == "aws_access_key_id")
            .unwrap();
        assert_eq!(aws.secret, "AKIAQ3EGRXPZ7K2LMN4D");
        assert_eq!((aws.start_line, aws.start_column), (1, 21));
        assert_eq!(aws.end_column, 41);
    }

    #[test]
    fn finds_own_tokens_and_private_keys() {
        let e = Engine::builtin();
        let pat = bgh_core::crypto::new_pat();
        assert_eq!(
            types(&e, &format!("x={pat}\n")),
            ["bgh_personal_access_token"]
        );
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n-----END OPENSSH PRIVATE KEY-----\n";
        let f = e.scan(key.as_bytes());
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].start_line, f[0].end_line), (1, 3));
    }

    #[test]
    fn skips_placeholders_binaries_and_non_provider_by_default() {
        let e = Engine::builtin();
        assert!(types(&e, "AKIAIOSFODNN7EXAMPLE").is_empty());
        assert!(e.scan(b"\0AKIAQ3EGRXPZ7K2LMN4D").is_empty());
        let conn = "DATABASE_URL=postgres://app:s3cr3tpw@db.internal/app\n";
        assert!(types(&e, conn).is_empty());
        let np = Engine::for_repo(true, vec![]);
        let f = np.scan(conn.as_bytes());
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].secret, "s3cr3tpw");
    }

    #[test]
    fn custom_patterns() {
        assert!(compile_custom("(").is_err());
        assert!(compile_custom("a*").is_err());
        assert!(compile_custom("").is_err());
        let re = compile_custom(r"ACME-[0-9]{6}").unwrap();
        let e = Engine::for_repo(
            false,
            vec![Pattern {
                secret_type: custom_secret_type(7),
                display_name: "ACME key".into(),
                regex: re,
                kind: Kind::Custom(7),
                push_protected: true,
            }],
        );
        assert_eq!(types(&e, "key ACME-123456 end"), ["custom_pattern_7"]);
        assert_eq!(e.push_protected().patterns.len(), e.patterns.len());
    }

    /// Throughput of the matcher alone over 100 MB of source-like text
    /// (run with `--ignored --nocapture` in release mode).
    #[test]
    #[ignore]
    fn throughput_100mb() {
        let e = Engine::builtin();
        let line =
            b"    let value = compute(some_identifier, 42, \"a string literal\"); // comment\n";
        let mut data = Vec::with_capacity(100 << 20);
        while data.len() < 100 << 20 {
            data.extend_from_slice(line);
        }
        data.extend_from_slice(b"AKIAQ3EGRXPZ7K2LMN4D\n");
        let t = std::time::Instant::now();
        let mut n = 0;
        for chunk in data.chunks(1 << 20) {
            n += e.scan(chunk).len();
        }
        let secs = t.elapsed().as_secs_f64();
        println!(
            "scanned 100 MB in {secs:.2}s ({:.0} MB/s), {n} findings",
            100.0 / secs
        );
        assert!(n >= 1);
    }
}
