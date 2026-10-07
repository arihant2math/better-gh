//! Process configuration, read from environment variables.
//!
//! Every setting has a default suitable for local development. Variables are
//! `BGH_`-prefixed except the conventional `DATABASE_URL` / `REDIS_URL`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;

use crate::settings::{OidcProvider, RateLimitSettings};

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
    /// `BGH_SMTP_URL` (e.g. `smtps://user:pass@smtp.example.com:465`,
    /// `smtp://localhost:25`). Unset: mail is logged and written to
    /// `{data_dir}/mail/` (see [`crate::mail`]).
    pub smtp_url: Option<String>,
    /// `BGH_MAIL_FROM` (default `Better GitHub <noreply@{hostname}>`).
    pub mail_from: Option<String>,
    /// API rate-limit defaults (`BGH_RATE_LIMIT*`, see [`crate::ratelimit`]);
    /// the `rate_limits` site setting overrides them field by field:
    /// `BGH_RATE_LIMIT_ENABLED` (default false: budgets are counted and
    /// reported, not enforced), `BGH_RATE_LIMIT` (core requests per hour per
    /// user, 5000; `0` = not enforced), `BGH_RATE_LIMIT_ANONYMOUS` (core per
    /// hour per IP, 60), `BGH_RATE_LIMIT_SEARCH` /
    /// `BGH_RATE_LIMIT_SEARCH_ANONYMOUS` (search per minute, 30 / 10),
    /// `BGH_RATE_LIMIT_GRAPHQL` (GraphQL per hour per user, 5000).
    pub rate_limits: RateLimitSettings,
    /// Single OIDC provider from `BGH_OIDC_*` (see `bgh_accounts::sso`), the
    /// default of the `auth_providers.oidc` site setting.
    pub oidc: Option<OidcProvider>,
    /// `BGH_TRUST_PROXY` (default false): take the client IP from
    /// `X-Forwarded-For` / `X-Real-IP` (set when behind a reverse proxy).
    pub trust_proxy: bool,
    /// `BGH_WEBHOOK_ALLOWED_HOSTS` (comma separated, default empty): hosts,
    /// IPs or CIDR ranges webhooks may target even though they resolve to
    /// private/loopback addresses. `*` allows everything.
    pub webhook_allowed_hosts: Vec<String>,
    /// `BGH_WEBHOOK_TIMEOUT_SECS` (default 10): per-delivery HTTP timeout.
    pub webhook_timeout_secs: u64,
    /// `BGH_ACTIONS_*` settings (CI, see [`ActionsConfig`]).
    pub actions: ActionsConfig,
    /// `BGH_EVENT_RETENTION_DAYS` (default 7): processed event outbox rows
    /// are kept this long (redelivery window, debugging).
    pub event_retention_days: i64,
    /// `BGH_SHUTDOWN_TIMEOUT_SECS` (default 30): on SIGTERM, how long to wait
    /// for in-flight HTTP requests before stopping background work.
    pub shutdown_timeout_secs: u64,
    /// `BGH_METRICS_TOKEN`: when set, `GET /metrics` on the main listener
    /// serves Prometheus metrics to `Authorization: Bearer <token>` (401
    /// otherwise). Unset (default): `/metrics` is not served there.
    pub metrics_token: Option<String>,
    /// `BGH_METRICS_LISTEN` (e.g. `127.0.0.1:9090`): a separate listener
    /// serving only `/metrics`, without a token unless `BGH_METRICS_TOKEN`
    /// is also set. Unset (default): no metrics listener.
    pub metrics_listen: Option<SocketAddr>,
    /// `BGH_SYNC_*`: sync log retention and WebSocket origins (bgh-sync).
    pub sync: SyncConfig,
    /// `HOSTNAME` (default `bgh`): names this process in job worker ids and
    /// the built-in Actions runner (`bgh-builtin-{instance_name}`).
    pub instance_name: String,
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
            smtp_url: None,
            mail_from: None,
            rate_limits: RateLimitSettings::default(),
            oidc: None,
            trust_proxy: false,
            webhook_allowed_hosts: Vec::new(),
            webhook_timeout_secs: 10,
            actions: ActionsConfig::default(),
            event_retention_days: 7,
            shutdown_timeout_secs: 30,
            metrics_token: None,
            metrics_listen: None,
            sync: SyncConfig::default(),
            instance_name: "bgh".into(),
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
            smtp_url: parse("BGH_SMTP_URL")?,
            mail_from: parse("BGH_MAIL_FROM")?,
            rate_limits: {
                let mut r = d.rate_limits.clone();
                r.enabled = boolean("BGH_RATE_LIMIT_ENABLED", r.enabled)?;
                let core = typed(
                    "BGH_RATE_LIMIT",
                    parse("BGH_RATE_LIMIT")?,
                    r.authenticated_per_hour,
                )?;
                if core <= 0 {
                    // Legacy spelling of "don't enforce".
                    r.enabled = false;
                } else {
                    r.authenticated_per_hour = core;
                }
                let positive = |key: &str, default: i64| -> anyhow::Result<i64> {
                    let v = typed(key, parse(key)?, default)?;
                    anyhow::ensure!(v > 0, "{key} must be positive");
                    Ok(v)
                };
                r.unauthenticated_per_hour =
                    positive("BGH_RATE_LIMIT_ANONYMOUS", r.unauthenticated_per_hour)?;
                r.search_authenticated_per_minute =
                    positive("BGH_RATE_LIMIT_SEARCH", r.search_authenticated_per_minute)?;
                r.search_unauthenticated_per_minute = positive(
                    "BGH_RATE_LIMIT_SEARCH_ANONYMOUS",
                    r.search_unauthenticated_per_minute,
                )?;
                r.graphql_per_hour = positive("BGH_RATE_LIMIT_GRAPHQL", r.graphql_per_hour)?;
                r
            },
            oidc: match (parse("BGH_OIDC_ISSUER")?, parse("BGH_OIDC_CLIENT_ID")?) {
                (Some(issuer), Some(client_id)) => Some(OidcProvider {
                    name: parse("BGH_OIDC_ID")?.unwrap_or_else(|| "oidc".into()),
                    display_name: Some(
                        parse("BGH_OIDC_NAME")?.unwrap_or_else(|| "Single sign-on".into()),
                    ),
                    issuer,
                    client_id,
                    client_secret: parse("BGH_OIDC_CLIENT_SECRET")?,
                    scopes: parse("BGH_OIDC_SCOPES")?
                        .map(|v| v.split_whitespace().map(str::to_string).collect())
                        .unwrap_or_default(),
                    auto_create_users: boolean("BGH_OIDC_AUTO_CREATE", true)?,
                    login_claim: parse("BGH_OIDC_LOGIN_CLAIM")?,
                    allowed_domains: parse("BGH_OIDC_ALLOWED_DOMAINS")?
                        .map(|v| {
                            v.split(',')
                                .map(|d| d.trim().to_lowercase())
                                .filter(|d| !d.is_empty())
                                .collect()
                        })
                        .unwrap_or_default(),
                    groups_claim: parse("BGH_OIDC_GROUPS_CLAIM")?,
                }),
                _ => None,
            },
            trust_proxy: boolean("BGH_TRUST_PROXY", d.trust_proxy)?,
            webhook_allowed_hosts: parse("BGH_WEBHOOK_ALLOWED_HOSTS")?
                .map(|v| {
                    v.split(',')
                        .map(|h| h.trim().to_string())
                        .filter(|h| !h.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            webhook_timeout_secs: typed(
                "BGH_WEBHOOK_TIMEOUT_SECS",
                parse("BGH_WEBHOOK_TIMEOUT_SECS")?,
                d.webhook_timeout_secs,
            )?,
            actions: ActionsConfig::from_lookup(&get)?,
            event_retention_days: typed(
                "BGH_EVENT_RETENTION_DAYS",
                parse("BGH_EVENT_RETENTION_DAYS")?,
                d.event_retention_days,
            )?,
            shutdown_timeout_secs: typed(
                "BGH_SHUTDOWN_TIMEOUT_SECS",
                parse("BGH_SHUTDOWN_TIMEOUT_SECS")?,
                d.shutdown_timeout_secs,
            )?,
            metrics_token: parse("BGH_METRICS_TOKEN")?.map(|t| t.trim().to_string()),
            metrics_listen: parse("BGH_METRICS_LISTEN")?
                .map(|v| {
                    v.trim()
                        .parse()
                        .with_context(|| format!("invalid value for BGH_METRICS_LISTEN: {v:?}"))
                })
                .transpose()?,
            sync: SyncConfig::from_lookup(&get),
            instance_name: parse("HOSTNAME")?
                .map(|h| h.trim().to_string())
                .unwrap_or(d.instance_name),
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

    /// `From:` address for outgoing mail.
    pub fn mail_from(&self) -> String {
        self.mail_from
            .clone()
            .unwrap_or_else(|| format!("{} <noreply@{}>", self.site_name, self.hostname()))
    }

    /// Directory holding bare git repositories.
    pub fn repos_dir(&self) -> PathBuf {
        self.data_dir.join("repos")
    }
}

/// GitHub Actions compatible CI (`bgh-actions`).
#[derive(Debug, Clone)]
pub struct ActionsConfig {
    /// `BGH_ACTIONS_ENABLED` (default true). Triggering of workflows.
    pub enabled: bool,
    /// `BGH_ACTIONS_BUILTIN_RUNNER` (default true). Run jobs in-process.
    pub builtin_runner: bool,
    /// `BGH_ACTIONS_EXECUTOR`: `auto` (default: docker when usable; the
    /// built-in runner takes no jobs otherwise), `docker` or `shell` (runs
    /// steps directly on the server host: trusted single-tenant installs
    /// only).
    pub executor: String,
    /// `BGH_ACTIONS_DEFAULT_IMAGE` (default `catthehacker/ubuntu:act-latest`):
    /// image for jobs without `container:` under the docker executor.
    pub default_image: String,
    /// `BGH_ACTIONS_MAX_JOBS` (default 2): concurrent jobs of the built-in runner.
    pub max_jobs: usize,
    /// `BGH_ACTIONS_RUNNER_LABELS` (comma separated): labels of the built-in runner.
    pub runner_labels: Vec<String>,
    /// `BGH_ACTIONS_WORK_DIR` (default `{tmp}/bgh-actions-work`; never
    /// inside `data_dir`).
    pub work_dir: Option<PathBuf>,
    /// `BGH_ACTIONS_SECRET_KEY`: base64 of 32 bytes encrypting secrets at
    /// rest. When unset, a key is generated in `{data_dir}/actions/server.key`.
    pub secret_key: Option<String>,
    /// `BGH_ACTIONS_ARTIFACT_RETENTION_DAYS` (default 90).
    pub artifact_retention_days: i64,
    /// `BGH_ACTIONS_CACHE_SIZE_LIMIT_GB` (default 10): default per-repository
    /// Actions cache size (bytes here); least recently used entries are
    /// evicted beyond it. Repositories may lower it (`cache/usage-policy`).
    pub cache_size_limit: u64,
    /// `BGH_ACTIONS_CACHE_RETENTION_DAYS` (default 7): cache entries not
    /// accessed for this long are deleted.
    pub cache_retention_days: i64,
    /// `BGH_ACTIONS_REMOTE_ACTIONS` (default true): fetch `owner/repo@ref`
    /// actions missing on this server from `BGH_ACTIONS_GITHUB_URL`
    /// (default `https://github.com`).
    pub remote_actions: bool,
    pub github_url: String,
    /// `BGH_DOCKER_BIN` (default `docker`).
    pub docker_bin: String,
}

impl Default for ActionsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            builtin_runner: true,
            executor: "auto".into(),
            default_image: "catthehacker/ubuntu:act-latest".into(),
            max_jobs: 2,
            runner_labels: [
                "self-hosted",
                "linux",
                "x64",
                "ubuntu-latest",
                "ubuntu-24.04",
                "ubuntu-22.04",
            ]
            .map(String::from)
            .to_vec(),
            work_dir: None,
            secret_key: None,
            artifact_retention_days: 90,
            cache_size_limit: 10 << 30,
            cache_retention_days: 7,
            remote_actions: true,
            github_url: "https://github.com".into(),
            docker_bin: "docker".into(),
        }
    }
}

impl ActionsConfig {
    fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let d = Self::default();
        let val = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let boolean = |k: &str, default: bool| -> anyhow::Result<bool> {
            match val(k) {
                None => Ok(default),
                Some(s) => match s.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => Ok(true),
                    "0" | "false" | "no" | "off" => Ok(false),
                    _ => anyhow::bail!("invalid boolean for {k}: {s:?}"),
                },
            }
        };
        let executor = val("BGH_ACTIONS_EXECUTOR").unwrap_or(d.executor);
        if !matches!(executor.as_str(), "auto" | "docker" | "shell") {
            anyhow::bail!("invalid value for BGH_ACTIONS_EXECUTOR: {executor:?}");
        }
        Ok(Self {
            enabled: boolean("BGH_ACTIONS_ENABLED", d.enabled)?,
            builtin_runner: boolean("BGH_ACTIONS_BUILTIN_RUNNER", d.builtin_runner)?,
            executor,
            default_image: val("BGH_ACTIONS_DEFAULT_IMAGE").unwrap_or(d.default_image),
            max_jobs: match val("BGH_ACTIONS_MAX_JOBS") {
                None => d.max_jobs,
                Some(s) => s
                    .trim()
                    .parse()
                    .with_context(|| format!("invalid value for BGH_ACTIONS_MAX_JOBS: {s:?}"))?,
            },
            runner_labels: val("BGH_ACTIONS_RUNNER_LABELS")
                .map(|s| {
                    s.split(',')
                        .map(|l| l.trim().to_ascii_lowercase())
                        .filter(|l| !l.is_empty())
                        .collect()
                })
                .unwrap_or(d.runner_labels),
            work_dir: val("BGH_ACTIONS_WORK_DIR").map(PathBuf::from),
            secret_key: val("BGH_ACTIONS_SECRET_KEY"),
            artifact_retention_days: match val("BGH_ACTIONS_ARTIFACT_RETENTION_DAYS") {
                None => d.artifact_retention_days,
                Some(s) => s.trim().parse().with_context(|| {
                    format!("invalid value for BGH_ACTIONS_ARTIFACT_RETENTION_DAYS: {s:?}")
                })?,
            },
            cache_size_limit: match val("BGH_ACTIONS_CACHE_SIZE_LIMIT_GB") {
                None => d.cache_size_limit,
                Some(s) => {
                    let gb: f64 = s.trim().parse().with_context(|| {
                        format!("invalid value for BGH_ACTIONS_CACHE_SIZE_LIMIT_GB: {s:?}")
                    })?;
                    anyhow::ensure!(gb > 0.0, "BGH_ACTIONS_CACHE_SIZE_LIMIT_GB must be positive");
                    (gb * (1u64 << 30) as f64) as u64
                }
            },
            cache_retention_days: match val("BGH_ACTIONS_CACHE_RETENTION_DAYS") {
                None => d.cache_retention_days,
                Some(s) => s.trim().parse().with_context(|| {
                    format!("invalid value for BGH_ACTIONS_CACHE_RETENTION_DAYS: {s:?}")
                })?,
            },
            remote_actions: boolean("BGH_ACTIONS_REMOTE_ACTIONS", d.remote_actions)?,
            github_url: val("BGH_ACTIONS_GITHUB_URL")
                .map(|s| s.trim_end_matches('/').to_string())
                .unwrap_or(d.github_url),
            docker_bin: val("BGH_DOCKER_BIN").unwrap_or(d.docker_bin),
        })
    }
}

/// bgh-sync settings (`Config::sync`).
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// `BGH_SYNC_RETENTION_HOURS` (168): sync actions older than this are
    /// pruned by the compaction job; clients further behind rebootstrap.
    pub retention: Duration,
    /// `BGH_SYNC_KEEP_LATEST` (false): instead of truncating, keep the latest
    /// action of every row (clients can always resume; the log stays bounded
    /// by the number of rows).
    pub keep_latest: bool,
    /// `BGH_SYNC_COMPACT_INTERVAL_SECS` (3600, at least 60): how often
    /// compaction runs.
    pub compact_interval: Duration,
    /// `BGH_SYNC_ALLOWED_ORIGINS` (empty): extra comma-separated `Origin`s
    /// accepted for cookie-authenticated WebSockets besides `BGH_BASE_URL`
    /// (e.g. the Vite dev server `http://localhost:5173`).
    pub allowed_origins: Vec<String>,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self::from_lookup(|_| None)
    }
}

impl SyncConfig {
    /// Lenient: unparsable numbers fall back to the default.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let num = |k: &str, d: u64| {
            get(k)
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(d)
        };
        Self {
            retention: Duration::from_secs(num("BGH_SYNC_RETENTION_HOURS", 168) * 3600),
            keep_latest: get("BGH_SYNC_KEEP_LATEST")
                .is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes" | "on")),
            compact_interval: Duration::from_secs(
                num("BGH_SYNC_COMPACT_INTERVAL_SECS", 3600).max(60),
            ),
            allowed_origins: get("BGH_SYNC_ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
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
    fn rate_limits_and_oidc() {
        let d = Config::from_lookup(|_| None).unwrap();
        assert!(!d.rate_limits.enabled);
        assert_eq!(d.rate_limits.authenticated_per_hour, 5000);
        assert!(d.oidc.is_none());
        let c = Config::from_lookup(|k| match k {
            "BGH_RATE_LIMIT_ENABLED" => Some("true".into()),
            "BGH_RATE_LIMIT" => Some("100".into()),
            "BGH_RATE_LIMIT_SEARCH_ANONYMOUS" => Some("2".into()),
            "BGH_OIDC_ISSUER" => Some("https://id.example.com".into()),
            "BGH_OIDC_CLIENT_ID" => Some("bgh".into()),
            "BGH_OIDC_SCOPES" => Some("openid email".into()),
            "BGH_OIDC_AUTO_CREATE" => Some("no".into()),
            _ => None,
        })
        .unwrap();
        assert!(c.rate_limits.enabled);
        assert_eq!(c.rate_limits.authenticated_per_hour, 100);
        assert_eq!(c.rate_limits.unauthenticated_per_hour, 60);
        assert_eq!(c.rate_limits.search_unauthenticated_per_minute, 2);
        let p = c.oidc.unwrap();
        assert_eq!((p.name.as_str(), p.client_id.as_str()), ("oidc", "bgh"));
        assert_eq!(p.scopes, ["openid", "email"]);
        assert!(!p.auto_create_users);
        // Legacy `BGH_RATE_LIMIT=0`: not enforced.
        let off = Config::from_lookup(|k| match k {
            "BGH_RATE_LIMIT_ENABLED" => Some("1".into()),
            "BGH_RATE_LIMIT" => Some("0".into()),
            _ => None,
        })
        .unwrap();
        assert!(!off.rate_limits.enabled);
        assert_eq!(off.rate_limits.authenticated_per_hour, 5000);
    }

    #[test]
    fn rejects_bad_values() {
        assert!(Config::from_lookup(|k| (k == "BGH_SSH_PORT").then(|| "x".into())).is_err());
    }

    #[test]
    fn sync_and_instance_name() {
        let d = Config::from_lookup(|_| None).unwrap();
        assert_eq!(d.sync.retention, Duration::from_secs(7 * 24 * 3600));
        assert!(!d.sync.keep_latest);
        assert_eq!(d.sync.compact_interval, Duration::from_secs(3600));
        assert_eq!(d.instance_name, "bgh");
        let c = Config::from_lookup(|k| match k {
            "BGH_SYNC_RETENTION_HOURS" => Some("2".into()),
            "BGH_SYNC_KEEP_LATEST" => Some("true".into()),
            "BGH_SYNC_COMPACT_INTERVAL_SECS" => Some("5".into()),
            "BGH_SYNC_ALLOWED_ORIGINS" => Some("http://a:1/, http://b".into()),
            "HOSTNAME" => Some("web-1".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(c.sync.retention, Duration::from_secs(7200));
        assert!(c.sync.keep_latest);
        assert_eq!(c.sync.compact_interval, Duration::from_secs(60));
        assert_eq!(c.sync.allowed_origins, vec!["http://a:1", "http://b"]);
        assert_eq!(c.instance_name, "web-1");
        let blank = Config::from_lookup(|k| (k == "HOSTNAME").then(|| " ".into())).unwrap();
        assert_eq!(blank.instance_name, "bgh");
    }

    /// `BGH_*` variables that are not operator settings: set by the server
    /// for its own git hooks, or used only by developers and tests.
    const INTERNAL_ENV: &[&str] = &[
        // pre-receive hook protocol (bgh_git::smart_http)
        "BGH_CHECK_DIR",
        "BGH_NO_FF_REFS",
        "BGH_LINEAR_REFS",
        "BGH_MAX_BLOB",
        "BGH_WARN_BLOB",
        "BGH_QUOTA_KB",
        "BGH_QUOTA_MESSAGE",
        "BGH_WORKFLOW_DENIED",
        // tests (bgh_core::testing)
        "BGH_TEST_KEEP_DB",
    ];

    /// Every `"BGH_…"` string literal in crate sources (not `tests/` or
    /// unit-test modules).
    fn env_literals() -> std::collections::BTreeMap<String, PathBuf> {
        fn walk(dir: &std::path::Path, out: &mut std::collections::BTreeMap<String, PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy();
                if path.is_dir() {
                    if name != "tests" && name != "target" {
                        walk(&path, out);
                    }
                } else if name.ends_with(".rs") {
                    let src = std::fs::read_to_string(&path).unwrap();
                    // Unit-test modules (like this one) don't read settings.
                    let src = src
                        .find("\n#[cfg(test)]\nmod ")
                        .map_or(&src[..], |e| &src[..e]);
                    for (i, _) in src.match_indices("\"BGH_") {
                        let rest = &src[i + 1..];
                        let end = rest
                            .find(|c: char| {
                                !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                            })
                            .unwrap_or(rest.len());
                        if rest[end..].starts_with('"') {
                            out.entry(rest[..end].to_string()).or_insert(path.clone());
                        }
                    }
                }
            }
        }
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut out = std::collections::BTreeMap::new();
        walk(&crates, &mut out);
        out
    }

    /// Drift guard (#226): every `BGH_*` variable the code reads is listed
    /// in SELF_HOSTING.md "Configuration reference" as `` `NAME` ``.
    #[test]
    fn env_vars_are_documented() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let doc = std::fs::read_to_string(root.join("docs/SELF_HOSTING.md")).unwrap();
        let start = doc
            .find("\n## Configuration reference\n")
            .expect("Configuration section");
        let section = &doc[start..];
        let section = &section[..section[1..].find("\n## ").map_or(section.len(), |e| e + 1)];
        let literals = env_literals();
        let missing: Vec<String> = literals
            .iter()
            .filter(|(name, _)| !INTERNAL_ENV.contains(&name.as_str()))
            .filter(|(name, _)| !section.contains(&format!("`{name}`")))
            .map(|(name, path)| format!("{name} ({})", path.display()))
            .collect();
        assert!(
            missing.is_empty(),
            "document these in docs/SELF_HOSTING.md \"Configuration reference\" (or add them to \
             INTERNAL_ENV if they are not settings): {missing:#?}"
        );
        let stale: Vec<_> = INTERNAL_ENV
            .iter()
            .filter(|name| !literals.contains_key(**name))
            .collect();
        assert!(
            stale.is_empty(),
            "INTERNAL_ENV lists unused names: {stale:?}"
        );
    }
}
