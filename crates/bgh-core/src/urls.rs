//! Builders for GitHub-style URLs (`url`, `html_url`, `*_url` templates).
//!
//! `url` fields point at the REST API (`{base}/api/v3/...`), `html_url`
//! fields at the web UI (`{base}/...`). Always build URLs here so every
//! crate agrees on the format. Access via `state.urls`.

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct Urls {
    /// `http://localhost:3000`
    pub base: String,
    /// `http://localhost:3000/api/v3`
    pub api: String,
    /// `localhost` — host used in SSH clone URLs.
    pub ssh_host: String,
    pub ssh_port: u16,
    /// `localhost:3000` — host used in `git://` URLs.
    pub host: String,
}

/// Percent-encode a single path segment (labels, branch names in paths, ...).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Percent-encode a path, keeping `/` separators (file paths, refs).
pub fn encode_path(s: &str) -> String {
    s.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

impl Urls {
    pub fn new(config: &Config) -> Self {
        Self {
            base: config.base_url.trim_end_matches('/').to_string(),
            api: config.api_url(),
            ssh_host: config.hostname().to_string(),
            ssh_port: config.ssh_port,
            host: config.host().to_string(),
        }
    }

    /// `{api}{path}`; `path` starts with `/`.
    pub fn api(&self, path: &str) -> String {
        format!("{}{}", self.api, path)
    }

    /// `{base}{path}`; `path` starts with `/`.
    pub fn html(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    // ----- users & orgs -------------------------------------------------

    pub fn user(&self, login: &str) -> String {
        format!("{}/users/{login}", self.api)
    }

    pub fn user_html(&self, login: &str) -> String {
        format!("{}/{login}", self.base)
    }

    /// Avatar URL: the stored custom avatar or the built-in generated one.
    /// Custom values starting with `/` are relative to the base URL.
    pub fn avatar(&self, user_id: i64, custom: Option<&str>) -> String {
        match custom {
            Some(path) if path.starts_with('/') => format!("{}{path}", self.base),
            Some(url) if !url.is_empty() => url.to_string(),
            _ => format!("{}/avatars/u/{user_id}?v=4", self.base),
        }
    }

    pub fn org(&self, login: &str) -> String {
        format!("{}/orgs/{login}", self.api)
    }

    pub fn team(&self, org_id: i64, team_id: i64) -> String {
        format!("{}/organizations/{org_id}/team/{team_id}", self.api)
    }

    pub fn team_html(&self, org_login: &str, slug: &str) -> String {
        format!("{}/orgs/{org_login}/teams/{slug}", self.base)
    }

    // ----- repositories -------------------------------------------------

    /// `{api}/repos/{owner}/{repo}`
    pub fn repo(&self, owner: &str, repo: &str) -> String {
        format!("{}/repos/{owner}/{repo}", self.api)
    }

    /// `{base}/{owner}/{repo}`
    pub fn repo_html(&self, owner: &str, repo: &str) -> String {
        format!("{}/{owner}/{repo}", self.base)
    }

    /// HTTPS clone URL `{base}/{owner}/{repo}.git`
    pub fn clone_url(&self, owner: &str, repo: &str) -> String {
        format!("{}/{owner}/{repo}.git", self.base)
    }

    /// SSH clone URL. Uses scp-like syntax on port 22, `ssh://` otherwise.
    pub fn ssh_url(&self, owner: &str, repo: &str) -> String {
        if self.ssh_port == 22 {
            format!("git@{}:{owner}/{repo}.git", self.ssh_host)
        } else {
            format!(
                "ssh://git@{}:{}/{owner}/{repo}.git",
                self.ssh_host, self.ssh_port
            )
        }
    }

    pub fn git_url(&self, owner: &str, repo: &str) -> String {
        format!("git://{}/{owner}/{repo}.git", self.host)
    }

    pub fn issue(&self, owner: &str, repo: &str, number: i64) -> String {
        format!("{}/repos/{owner}/{repo}/issues/{number}", self.api)
    }

    pub fn issue_html(&self, owner: &str, repo: &str, number: i64) -> String {
        format!("{}/{owner}/{repo}/issues/{number}", self.base)
    }

    pub fn pull(&self, owner: &str, repo: &str, number: i64) -> String {
        format!("{}/repos/{owner}/{repo}/pulls/{number}", self.api)
    }

    pub fn pull_html(&self, owner: &str, repo: &str, number: i64) -> String {
        format!("{}/{owner}/{repo}/pull/{number}", self.base)
    }

    pub fn issue_comment(&self, owner: &str, repo: &str, id: i64) -> String {
        format!("{}/repos/{owner}/{repo}/issues/comments/{id}", self.api)
    }

    pub fn issue_comment_html(&self, owner: &str, repo: &str, number: i64, id: i64) -> String {
        format!(
            "{}/{owner}/{repo}/issues/{number}#issuecomment-{id}",
            self.base
        )
    }

    pub fn label(&self, owner: &str, repo: &str, name: &str) -> String {
        format!(
            "{}/repos/{owner}/{repo}/labels/{}",
            self.api,
            encode_segment(name)
        )
    }

    pub fn milestone(&self, owner: &str, repo: &str, number: i64) -> String {
        format!("{}/repos/{owner}/{repo}/milestones/{number}", self.api)
    }

    pub fn milestone_html(&self, owner: &str, repo: &str, number: i64) -> String {
        format!("{}/{owner}/{repo}/milestone/{number}", self.base)
    }

    pub fn commit(&self, owner: &str, repo: &str, sha: &str) -> String {
        format!("{}/repos/{owner}/{repo}/commits/{sha}", self.api)
    }

    pub fn commit_html(&self, owner: &str, repo: &str, sha: &str) -> String {
        format!("{}/{owner}/{repo}/commit/{sha}", self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(port: u16) -> Urls {
        Urls::new(&Config {
            ssh_port: port,
            ..Config::default()
        })
    }

    #[test]
    fn builds_urls() {
        let u = urls(2222);
        assert_eq!(u.repo("o", "r"), "http://localhost:3000/api/v3/repos/o/r");
        assert_eq!(u.clone_url("o", "r"), "http://localhost:3000/o/r.git");
        assert_eq!(u.ssh_url("o", "r"), "ssh://git@localhost:2222/o/r.git");
        assert_eq!(urls(22).ssh_url("o", "r"), "git@localhost:o/r.git");
        assert_eq!(
            u.label("o", "r", "good first issue"),
            "http://localhost:3000/api/v3/repos/o/r/labels/good%20first%20issue"
        );
        assert_eq!(encode_path("a b/c.txt"), "a%20b/c.txt");
    }
}
