//! On-disk repository storage.

use std::path::{Path, PathBuf};

use bgh_core::config::Config;

use crate::cmd;
use crate::read::GitRepo;
use crate::{GitError, GitResult};

/// Version stamped into every repository's config as `bgh.configVersion`;
/// bump it whenever [`REPO_CONFIG`] changes so [`RepoStore::upgrade_config`]
/// rewrites existing repositories (1 = configs written before the stamp).
pub const CONFIG_VERSION: u32 = 2;

/// Settings appended to every repository's `config`.
///
/// * `receive.hideRefs`: `refs/pull/*` (PR heads and test merges) and
///   `refs/bgh/*` are written by the server only; pushes to them fail with
///   `deny updating a hidden ref`.
/// * `receive.fsckObjects`: reject malformed objects, malicious
///   `.gitmodules` and symlinks into `.git` (site setting
///   `git.fsck_on_push` overrides it per push with `-c`). Common harmless
///   defects in old history are downgraded to warnings.
const REPO_CONFIG: &str = "\
[core]
\tlogAllRefUpdates = false
[receive]
\tadvertisePushOptions = true
\tautogc = false
\tunpackLimit = 100
\tfsckObjects = true
\thideRefs = refs/pull/
\thideRefs = refs/bgh/
[receive \"fsck\"]
\tzeroPaddedFilemode = ignore
\tbadTimezone = ignore
\tmissingSpaceBeforeDate = ignore
[uploadpack]
\tallowFilter = true
\tallowReachableSHA1InWant = true
[gc]
\tauto = 0
[bgh]
\tconfigVersion = 2
";

/// Ref namespaces only the server may write (see [`REPO_CONFIG`]).
pub const HIDDEN_REF_PREFIXES: &[&str] = &["refs/pull/", "refs/bgh/"];

/// Whether `refname` is in a namespace pushes and the refs API may not
/// touch.
pub fn is_hidden_ref(refname: &str) -> bool {
    HIDDEN_REF_PREFIXES.iter().any(|p| refname.starts_with(p))
}

/// `(section, subsection)` of a config section header line
/// (`[core]`, `[receive "fsck"]`, legacy `[branch.main]`). Section names
/// are case-insensitive, subsections case-sensitive (legacy ones aren't).
fn config_header(line: &str) -> Option<(String, Option<String>)> {
    let inner = line.trim().strip_prefix('[')?;
    let inner = &inner[..inner.find(']')?];
    Some(match inner.split_once('"') {
        Some((name, rest)) => {
            let sub = rest.rsplit_once('"').map_or(rest, |(s, _)| s);
            (
                name.trim().to_ascii_lowercase(),
                Some(sub.replace("\\", "")),
            )
        }
        None => match inner.split_once('.') {
            Some((name, sub)) => (name.to_ascii_lowercase(), Some(sub.to_ascii_lowercase())),
            None => (inner.trim().to_ascii_lowercase(), None),
        },
    })
}

/// Lower-cased variable name of a config entry line (`None` for blank and
/// comment lines).
fn config_key(line: &str) -> Option<String> {
    let t = line.trim_start();
    if t.is_empty() || t.starts_with('#') || t.starts_with(';') || t.starts_with('[') {
        return None;
    }
    let end = t
        .find(|c: char| c == '=' || c.is_whitespace())
        .unwrap_or(t.len());
    Some(t[..end].to_ascii_lowercase())
}

type ConfigKey = (String, Option<String>, String);

/// `(section, subsection, key)` of every entry of `text`.
fn config_entries(text: &str) -> Vec<ConfigKey> {
    let mut section = None;
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(h) = config_header(line) {
            section = Some(h);
        } else if let (Some((s, sub)), Some(k)) = (&section, config_key(line)) {
            out.push((s.clone(), sub.clone(), k));
        }
    }
    out
}

/// `bgh.configVersion` of a config file's text (1 when absent).
pub fn config_version(text: &str) -> u32 {
    let mut in_bgh = false;
    let mut version = 1;
    for line in text.lines() {
        if let Some((s, sub)) = config_header(line) {
            in_bgh = s == "bgh" && sub.is_none();
        } else if in_bgh && config_key(line).as_deref() == Some("configversion") {
            version = line
                .split_once('=')
                .and_then(|(_, v)| v.trim().parse().ok())
                .unwrap_or(1);
        }
    }
    version
}

/// `existing` with every variable [`REPO_CONFIG`] sets removed (sections
/// left empty by that dropped), then [`REPO_CONFIG`] appended. Idempotent;
/// everything else (`core.bare`, alternates-related settings, user
/// additions) is kept verbatim.
pub fn rewrite_config(existing: &str) -> String {
    let managed = config_entries(REPO_CONFIG);
    let mut out = String::with_capacity(existing.len() + REPO_CONFIG.len());
    // (header line, kept lines, whether anything was dropped)
    let mut sections: Vec<(Option<&str>, Vec<&str>, bool)> = vec![(None, vec![], false)];
    let mut current: Option<(String, Option<String>)> = None;
    for line in existing.lines() {
        if let Some(h) = config_header(line) {
            current = Some(h);
            sections.push((Some(line), vec![], false));
            continue;
        }
        let last = sections.last_mut().expect("at least one section");
        let drop = match (&current, config_key(line)) {
            (Some((s, sub)), Some(k)) => managed.contains(&(s.clone(), sub.clone(), k)),
            _ => false,
        };
        if drop {
            last.2 = true;
        } else {
            last.1.push(line);
        }
    }
    for (header, lines, dropped) in sections {
        let has_entries = lines.iter().any(|l| config_key(l).is_some());
        if dropped && !has_entries {
            continue;
        }
        for l in header.into_iter().chain(lines) {
            out.push_str(l);
            out.push('\n');
        }
    }
    out.push_str(REPO_CONFIG);
    out
}

/// Where repositories live and how to reach the `git` binary.
#[derive(Debug, Clone)]
pub struct RepoStore {
    /// `{data_dir}/repos`
    pub root: PathBuf,
    pub git_bin: String,
    /// Blob size limit for API reads (bytes).
    pub max_blob_size: u64,
    /// Directory name suffix: `.git` for repositories, `.wiki.git` for
    /// their wikis (see [`Self::wiki`]).
    pub suffix: &'static str,
    /// Signs commits the server creates (`None`: they stay unsigned).
    pub signer: Option<std::sync::Arc<crate::signing::WebFlowKey>>,
}

/// The web-flow key in `{data_dir}/signing/` (generated on first use).
pub fn web_flow_signer(config: &Config) -> Option<std::sync::Arc<crate::signing::WebFlowKey>> {
    match crate::signing::web_flow_key(&config.data_dir.join("signing")) {
        Ok(k) => Some(k),
        Err(err) => {
            tracing::warn!(%err, "web-flow signing key unavailable; server commits stay unsigned");
            None
        }
    }
}

/// Directory suffix of main repositories.
pub const REPO_SUFFIX: &str = ".git";
/// Directory suffix of wiki repositories.
pub const WIKI_SUFFIX: &str = ".wiki.git";

impl RepoStore {
    pub fn new(root: impl Into<PathBuf>, git_bin: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            git_bin: git_bin.into(),
            max_blob_size: 10 * 1024 * 1024,
            suffix: REPO_SUFFIX,
            signer: None,
        }
    }

    pub fn from_config(config: &Config) -> Self {
        Self {
            root: config.repos_dir(),
            git_bin: config.git_bin.clone(),
            max_blob_size: config.max_blob_size,
            suffix: REPO_SUFFIX,
            signer: web_flow_signer(config),
        }
    }

    /// The same store addressing wiki repositories
    /// (`{root}/{id % 256 as 2-hex}/{id}.wiki.git`, next to the main repo).
    /// Every read/write/transport helper works unchanged on it.
    pub fn wiki(&self) -> RepoStore {
        RepoStore {
            suffix: WIKI_SUFFIX,
            ..self.clone()
        }
    }

    /// Whether this store addresses wiki repositories.
    pub fn is_wiki(&self) -> bool {
        self.suffix == WIKI_SUFFIX
    }

    /// `{root}/{id % 256 as 2-hex}/{id}{suffix}` (suffix `.git` by default)
    pub fn path(&self, repo_id: i64) -> PathBuf {
        self.root
            .join(format!("{:02x}", repo_id.rem_euclid(256)))
            .join(format!("{repo_id}{}", self.suffix))
    }

    pub fn exists(&self, repo_id: i64) -> bool {
        self.path(repo_id).join("HEAD").is_file()
    }

    async fn configure(&self, path: &Path) -> GitResult<()> {
        let cfg = path.join("config");
        let existing = tokio::fs::read_to_string(&cfg).await.unwrap_or_default();
        tokio::fs::write(&cfg, rewrite_config(&existing)).await?;
        Ok(())
    }

    /// Bring `git_dir`'s config up to [`CONFIG_VERSION`] (see
    /// [`rewrite_config`]); returns whether it was rewritten. The new file
    /// replaces the old one atomically. Blocking.
    pub fn upgrade_config(git_dir: &Path) -> GitResult<bool> {
        let cfg = git_dir.join("config");
        let existing = std::fs::read_to_string(&cfg)?;
        if config_version(&existing) >= CONFIG_VERSION {
            return Ok(false);
        }
        let tmp = git_dir.join(format!("config.bgh-{}", bgh_core::crypto::random_token(8)));
        std::fs::write(&tmp, rewrite_config(&existing))?;
        std::fs::rename(&tmp, &cfg)?;
        Ok(true)
    }

    /// Run [`Self::upgrade_config`] over every repository on disk (main and
    /// wiki repositories alike). Blocking; returns `(rewritten, failed)`.
    pub fn upgrade_all_configs(&self) -> (usize, usize) {
        let (mut rewritten, mut failed) = (0, 0);
        let Ok(shards) = std::fs::read_dir(&self.root) else {
            return (0, 0);
        };
        for shard in shards.flatten() {
            let name = shard.file_name();
            let name = name.to_string_lossy();
            if name.len() != 2 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let Ok(repos) = std::fs::read_dir(shard.path()) else {
                continue;
            };
            for repo in repos.flatten() {
                let path = repo.path();
                if !path.join("HEAD").is_file() || !path.join("config").is_file() {
                    continue;
                }
                match Self::upgrade_config(&path) {
                    Ok(true) => rewritten += 1,
                    Ok(false) => {}
                    Err(err) => {
                        failed += 1;
                        tracing::warn!(path = %path.display(), %err, "repository config upgrade failed");
                    }
                }
            }
        }
        (rewritten, failed)
    }

    /// Create an empty bare repository whose HEAD points at `default_branch`.
    pub async fn init(&self, repo_id: i64, default_branch: &str) -> GitResult<PathBuf> {
        if !crate::is_valid_ref_name(default_branch) {
            return Err(GitError::InvalidInput(format!(
                "invalid branch name {default_branch:?}"
            )));
        }
        let path = self.path(repo_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let p = path.to_string_lossy().to_string();
        let branch = format!("--initial-branch={default_branch}");
        cmd::run(
            &self.git_bin,
            None,
            &["init", "--bare", "--quiet", "--template=", &branch, &p],
            &[],
            None,
        )
        .await?;
        self.configure(&path).await?;
        Ok(path)
    }

    /// Create `dst_id` as a fork of `src_id`: a bare clone sharing objects
    /// through `objects/info/alternates`. Copies all branches and tags.
    ///
    /// Note: before deleting a repository that has forks, forks must be
    /// made self-contained (`git repack -a -d` + remove alternates).
    pub async fn fork(&self, src_id: i64, dst_id: i64) -> GitResult<PathBuf> {
        let src = self.path(src_id);
        if !self.exists(src_id) {
            return Err(GitError::NotFound(format!("repository {src_id}")));
        }
        let dst = self.path(dst_id);
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let (s, d) = (
            src.to_string_lossy().to_string(),
            dst.to_string_lossy().to_string(),
        );
        cmd::run(
            &self.git_bin,
            None,
            &[
                "clone",
                "--bare",
                "--shared",
                "--quiet",
                "--template=",
                &s,
                &d,
            ],
            &[],
            None,
        )
        .await?;
        cmd::run(
            &self.git_bin,
            Some(&dst),
            &["remote", "remove", "origin"],
            &[],
            None,
        )
        .await?;
        self.configure(&dst).await?;
        Ok(dst)
    }

    /// Remove a repository from disk (idempotent).
    pub async fn delete(&self, repo_id: i64) -> GitResult<()> {
        crate::cache::evict(&self.path(repo_id));
        match tokio::fs::remove_dir_all(self.path(repo_id)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Open for in-process reads (blocking; see [`Self::read`]).
    pub fn open(&self, repo_id: i64) -> GitResult<GitRepo> {
        if !self.exists(repo_id) {
            return Err(GitError::NotFound(format!("repository {repo_id}")));
        }
        Ok(
            GitRepo::open_cached(&self.path(repo_id), self.max_blob_size)?
                .with_git_bin(&self.git_bin),
        )
    }

    /// Run a blocking read against a repository on the blocking thread pool.
    ///
    /// ```ignore
    /// let branches = store.read(repo.id, |r| r.branches()).await?;
    /// ```
    pub async fn read<T, F>(&self, repo_id: i64, f: F) -> GitResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&GitRepo) -> GitResult<T> + Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let repo = store.open(repo_id)?;
            f(&repo)
        })
        .await
        .map_err(|e| GitError::Object(format!("blocking task failed: {e}")))?
    }

    /// Size of the repository on disk in KB (objects only).
    pub async fn disk_size_kb(&self, repo_id: i64) -> GitResult<i64> {
        let objects = self.path(repo_id).join("objects");
        tokio::task::spawn_blocking(move || {
            fn walk(p: &Path) -> u64 {
                let Ok(rd) = std::fs::read_dir(p) else {
                    return 0;
                };
                rd.flatten()
                    .map(|e| match e.file_type() {
                        Ok(t) if t.is_dir() => walk(&e.path()),
                        Ok(_) => e.metadata().map(|m| m.len()).unwrap_or(0),
                        Err(_) => 0,
                    })
                    .sum()
            }
            (walk(&objects) / 1024) as i64
        })
        .await
        .map_err(|e| GitError::Object(e.to_string()))
    }

    /// Path helper for transport code.
    pub fn git_dir(&self, repo_id: i64) -> GitResult<PathBuf> {
        if self.exists(repo_id) {
            Ok(self.path(repo_id))
        } else {
            Err(GitError::NotFound(format!("repository {repo_id}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = "[core]
\trepositoryformatversion = 0
\tfilemode = true
\tbare = true
[core]
\tlogAllRefUpdates = false
[receive]
\tadvertisePushOptions = true
\tautogc = false
\tunpackLimit = 100
[uploadpack]
\tallowFilter = true
\tallowReachableSHA1InWant = true
[gc]
\tauto = 0
[remote \"x\"]
\turl = /tmp/x
";

    #[test]
    fn rewrite_is_idempotent_and_keeps_foreign_settings() {
        assert_eq!(config_version(LEGACY), 1);
        let once = rewrite_config(LEGACY);
        assert_eq!(config_version(&once), CONFIG_VERSION);
        assert_eq!(rewrite_config(&once), once);
        assert!(once.contains("\tbare = true"));
        assert!(once.contains("[remote \"x\"]\n\turl = /tmp/x"));
        assert_eq!(once.matches("unpackLimit").count(), 1);
        assert_eq!(once.matches("hideRefs = refs/pull/").count(), 1);
        // The legacy `[core]` section whose only entry moved is dropped.
        assert_eq!(once.matches("[core]").count(), 2);
    }

    #[test]
    fn hidden_refs() {
        assert!(is_hidden_ref("refs/pull/1/head"));
        assert!(is_hidden_ref("refs/bgh/x"));
        assert!(!is_hidden_ref("refs/heads/pull/1"));
        assert!(!is_hidden_ref("refs/pullx"));
    }

    #[tokio::test]
    async fn upgrades_existing_repositories() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RepoStore::new(tmp.path(), "git");
        let path = store.init(7, "main").await.unwrap();
        std::fs::write(path.join("config"), LEGACY).unwrap();
        assert_eq!(store.upgrade_all_configs(), (1, 0));
        assert_eq!(store.upgrade_all_configs(), (0, 0));
        let out = std::process::Command::new("git")
            .args(["config", "--get-all", "receive.hideRefs"])
            .current_dir(&path)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "refs/pull/\nrefs/bgh/\n"
        );
        let out = std::process::Command::new("git")
            .args(["config", "receive.fsck.zeroPaddedFilemode"])
            .current_dir(&path)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "ignore\n");
    }
}
