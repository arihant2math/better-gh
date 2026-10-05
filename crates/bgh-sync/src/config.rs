//! bgh-sync settings, read once from the environment.
//!
//! * `BGH_SYNC_RETENTION_HOURS` (168): sync actions older than this are
//!   pruned by the compaction job; clients further behind rebootstrap.
//! * `BGH_SYNC_KEEP_LATEST` (false): instead of truncating, keep the latest
//!   action of every row (clients can always resume; the log stays bounded
//!   by the number of rows).
//! * `BGH_SYNC_COMPACT_INTERVAL_SECS` (3600): how often compaction runs.
//! * `BGH_SYNC_ALLOWED_ORIGINS` (empty): extra `Origin`s accepted for
//!   cookie-authenticated WebSockets besides `BGH_BASE_URL` (e.g. the Vite
//!   dev server `http://localhost:5173`).

use std::sync::OnceLock;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub retention: Duration,
    pub keep_latest: bool,
    pub compact_interval: Duration,
    pub allowed_origins: Vec<String>,
}

impl SyncConfig {
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

/// Process-wide settings.
pub fn get() -> &'static SyncConfig {
    static CONFIG: OnceLock<SyncConfig> = OnceLock::new();
    CONFIG.get_or_init(|| SyncConfig::from_lookup(|k| std::env::var(k).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let d = SyncConfig::from_lookup(|_| None);
        assert_eq!(d.retention, Duration::from_secs(7 * 24 * 3600));
        assert!(!d.keep_latest);
        let c = SyncConfig::from_lookup(|k| match k {
            "BGH_SYNC_RETENTION_HOURS" => Some("2".into()),
            "BGH_SYNC_KEEP_LATEST" => Some("true".into()),
            "BGH_SYNC_ALLOWED_ORIGINS" => Some("http://a:1/, http://b".into()),
            _ => None,
        });
        assert_eq!(c.retention, Duration::from_secs(7200));
        assert!(c.keep_latest);
        assert_eq!(c.allowed_origins, vec!["http://a:1", "http://b"]);
    }
}
