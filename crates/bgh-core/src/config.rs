//! Process configuration, read from environment variables.
//!
//! Every setting has a default suitable for local development. Variables are
//! `BGH_`-prefixed except the conventional `DATABASE_URL` / `REDIS_URL`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Context;

#[derive(Debug, Clone)]
pub struct Config {
    /// `BGH_LISTEN` (default `0.0.0.0:3000`).
    pub listen_addr: SocketAddr,
    /// `BGH_BASE_URL` (default `http://localhost:3000`). External URL used in
    /// all generated links. Never has a trailing slash.
    pub base_url: String,
    /// `DATABASE_URL`.
    pub database_url: String,
    /// `BGH_DB_MAX_CONNECTIONS` (default 20).
    pub db_max_connections: u32,
    /// `REDIS_URL`.
    pub redis_url: String,
    /// `BGH_REDIS_PREFIX` (default `bgh:`). Prepended to every Redis key and
    /// pub/sub channel (see [`crate::AppState::redis_key`]); tests use a
    /// unique prefix per test.
    pub redis_prefix: String,
    /// `BGH_DATA_DIR` (default `./data`). Git repositories live in
    /// `{data_dir}/repos`, uploads in `{data_dir}/files`.
    pub data_dir: PathBuf,
    /// `BGH_WEB_DIR` (default `web/dist`). Built web client.
    pub web_dir: PathBuf,
    /// `BGH_SSH_PORT` (default 2222). Advertised in `ssh_url`.
    pub ssh_port: u16,
    /// `BGH_SSH_ENABLED` (default true).
    pub ssh_enabled: bool,
    /// `BGH_SIGNUP_ENABLED` (default true). Allows self-service sign-up.
    pub signup_enabled: bool,
    /// `BGH_SESSION_TTL_DAYS` (default 30).
    pub session_ttl_days: i64,
    /// `BGH_JOB_WORKERS` (default 4). Concurrent job worker tasks; 0 disables.
    pub job_workers: usize,
    /// `BGH_GIT_BIN` (default `git`).
    pub git_bin: String,
    /// `BGH_MAX_BLOB_SIZE` in bytes (default 10 MiB). Larger blobs are not
    /// loaded into memory by API/rendering code.
    pub max_blob_size: u64,
    /// `BGH_SITE_NAME` (default `Better GitHub`).
    pub site_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:3000".parse().expect("valid default addr"),
            base_url: "http://localhost:3000".into(),
            database_url: "postgres://postgres:postgres@localhost/bgh".into(),
            db_max_connections: 20,
            redis_url: "redis://127.0.0.1/".into(),
            redis_prefix: "bgh:".into(),
            data_dir: PathBuf::from("./data"),
            web_dir: PathBuf::from("web/dist"),
            ssh_port: 2222,
            ssh_enabled: true,
            signup_enabled: true,
            session_ttl_days: 30,
            job_workers: 4,
            git_bin: "git".into(),
            max_blob_size: 10 * 1024 * 1024,
            site_name: "Better GitHub".into(),
        }
    }
}

impl Config {
    /// Read configuration from the process environment.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Read configuration through an arbitrary lookup (used by tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let d = Self::default();
        let parse = |key: &str| -> anyhow::Result<Option<String>> {
            Ok(get(key).filter(|v| !v.trim().is_empty()))
        };
        fn typed<T: FromStr>(key: &str, v: Option<String>, default: T) -> anyhow::Result<T>
        where
            T::Err: std::fmt::Display,
        {
            match v {
                None => Ok(default),
                Some(s) => s
                    .trim()
                    .parse::<T>()
                    .map_err(|e| anyhow::anyhow!("{e}"))
                    .with_context(|| format!("invalid value for {key}: {s:?}")),
            }
        }
        let boolean = |key: &str, default: bool| -> anyhow::Result<bool> {
            match parse(key)? {
                None => Ok(default),
                Some(s) => match s.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => Ok(true),
                    "0" | "false" | "no" | "off" => Ok(false),
                    _ => anyhow::bail!("invalid boolean for {key}: {s:?}"),
                },
            }
        };

        Ok(Self {
            listen_addr: typed("BGH_LISTEN", parse("BGH_LISTEN")?, d.listen_addr)?,
            base_url: parse("BGH_BASE_URL")?
                .map(|s| s.trim_end_matches('/').to_string())
                .unwrap_or(d.base_url),
            database_url: parse("DATABASE_URL")?.unwrap_or(d.database_url),
            db_max_connections: typed(
                "BGH_DB_MAX_CONNECTIONS",
                parse("BGH_DB_MAX_CONNECTIONS")?,
                d.db_max_connections,
            )?,
            redis_url: parse("REDIS_URL")?.unwrap_or(d.redis_url),
            redis_prefix: get("BGH_REDIS_PREFIX").unwrap_or(d.redis_prefix),
            data_dir: parse("BGH_DATA_DIR")?
                .map(PathBuf::from)
                .unwrap_or(d.data_dir),
            web_dir: parse("BGH_WEB_DIR")?
                .map(PathBuf::from)
                .unwrap_or(d.web_dir),
            ssh_port: typed("BGH_SSH_PORT", parse("BGH_SSH_PORT")?, d.ssh_port)?,
            ssh_enabled: boolean("BGH_SSH_ENABLED", d.ssh_enabled)?,
            signup_enabled: boolean("BGH_SIGNUP_ENABLED", d.signup_enabled)?,
            session_ttl_days: typed(
                "BGH_SESSION_TTL_DAYS",
                parse("BGH_SESSION_TTL_DAYS")?,
                d.session_ttl_days,
            )?,
            job_workers: typed("BGH_JOB_WORKERS", parse("BGH_JOB_WORKERS")?, d.job_workers)?,
            git_bin: parse("BGH_GIT_BIN")?.unwrap_or(d.git_bin),
            max_blob_size: typed(
                "BGH_MAX_BLOB_SIZE",
                parse("BGH_MAX_BLOB_SIZE")?,
                d.max_blob_size,
            )?,
            site_name: parse("BGH_SITE_NAME")?.unwrap_or(d.site_name),
        })
    }

    /// `{base_url}/api/v3`
    pub fn api_url(&self) -> String {
        format!("{}/api/v3", self.base_url)
    }

    /// Host part of the base URL (`localhost:3000`), used for SSH URLs.
    pub fn host(&self) -> &str {
        let rest = self
            .base_url
            .split_once("://")
            .map(|(_, r)| r)
            .unwrap_or(&self.base_url);
        rest.split('/').next().unwrap_or(rest)
    }

    /// Host without port (`localhost`).
    pub fn hostname(&self) -> &str {
        let host = self.host();
        if host.starts_with('[') {
            return host.split(']').next().map(|h| &h[1..]).unwrap_or(host);
        }
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
    }

    /// Whether cookies should carry the `Secure` attribute.
    pub fn secure_cookies(&self) -> bool {
        self.base_url.starts_with("https://")
    }

    /// Directory holding bare git repositories.
    pub fn repos_dir(&self) -> PathBuf {
        self.data_dir.join("repos")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let c = Config::from_lookup(|k| match k {
            "BGH_BASE_URL" => Some("https://git.example.com/".into()),
            "BGH_SIGNUP_ENABLED" => Some("false".into()),
            "BGH_SSH_PORT" => Some("22".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(c.base_url, "https://git.example.com");
        assert_eq!(c.api_url(), "https://git.example.com/api/v3");
        assert!(!c.signup_enabled);
        assert_eq!(c.ssh_port, 22);
        assert_eq!(c.listen_addr.port(), 3000);
        assert_eq!(c.hostname(), "git.example.com");
        assert!(c.secure_cookies());
        let d = Config::default();
        assert_eq!(d.host(), "localhost:3000");
        assert_eq!(d.hostname(), "localhost");
    }

    #[test]
    fn rejects_bad_values() {
        assert!(Config::from_lookup(|k| (k == "BGH_SSH_PORT").then(|| "x".into())).is_err());
    }
}
