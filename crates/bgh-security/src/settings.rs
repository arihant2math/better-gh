//! Per-repository `security_and_analysis` (secret scanning toggles), with
//! the site-wide `secret_scanning` settings applied.

use bgh_core::ApiResult;
use bgh_core::error::{ApiError, FieldError};
use bgh_core::state::AppState;
use serde_json::{Value, json};

/// Stored toggles of one repository (`repo_security_settings`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Stored {
    pub secret_scanning: bool,
    pub push_protection: bool,
    pub non_provider_patterns: bool,
}

impl Stored {
    pub async fn load(db: impl sqlx::PgExecutor<'_>, repo_id: i64) -> Result<Self, sqlx::Error> {
        Ok(sqlx::query_as(
            "SELECT secret_scanning, push_protection, non_provider_patterns
               FROM repo_security_settings WHERE repo_id = $1",
        )
        .bind(repo_id)
        .fetch_optional(db)
        .await?
        .unwrap_or_default())
    }
}

/// Effective settings of one repository.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Effective {
    /// Secret scanning is available on this site at all.
    pub available: bool,
    pub secret_scanning: bool,
    pub push_protection: bool,
    pub non_provider_patterns: bool,
    /// Forced on by the site administrator.
    pub forced_scanning: bool,
    pub forced_push_protection: bool,
    /// Largest blob scanned (bytes).
    pub max_blob: u64,
    pub push_timeout: std::time::Duration,
}

impl Effective {
    pub fn compute(site: &bgh_core::settings::SecretScanningSettings, stored: Stored) -> Self {
        let available = site.available;
        let secret_scanning = available && (site.enable_all || stored.secret_scanning);
        Self {
            available,
            secret_scanning,
            // Push protection needs secret scanning (like GitHub).
            push_protection: secret_scanning
                && (site.push_protection_all || stored.push_protection),
            non_provider_patterns: secret_scanning && stored.non_provider_patterns,
            forced_scanning: available && site.enable_all,
            forced_push_protection: available && site.push_protection_all,
            max_blob: site.max_blob_kb.max(1) as u64 * 1024,
            push_timeout: std::time::Duration::from_secs(site.push_scan_timeout_secs.max(1) as u64),
        }
    }
}

pub async fn effective(state: &AppState, repo_id: i64) -> ApiResult<Effective> {
    let site = bgh_core::settings::load(state).await?;
    let stored = Stored::load(&state.db, repo_id).await?;
    Ok(Effective::compute(&site.secret_scanning, stored))
}

/// Secret scanning must be on for alert endpoints: GitHub's 404.
pub fn require_enabled(e: &Effective) -> ApiResult<()> {
    if e.secret_scanning {
        Ok(())
    } else {
        Err(ApiError::Status(
            axum::http::StatusCode::NOT_FOUND,
            "Secret scanning is disabled on this repository.".into(),
        ))
    }
}

fn status(on: bool) -> Value {
    json!({"status": if on { "enabled" } else { "disabled" }})
}

/// The repository JSON's `security_and_analysis` (shown to admins).
pub async fn security_and_analysis(state: &AppState, repo_id: i64) -> ApiResult<Value> {
    let e = effective(state, repo_id).await?;
    Ok(json!({
        "advanced_security": status(false),
        "dependabot_security_updates": status(false),
        "secret_scanning": status(e.secret_scanning),
        "secret_scanning_push_protection": status(e.push_protection),
        "secret_scanning_non_provider_patterns": status(e.non_provider_patterns),
        "secret_scanning_validity_checks": status(false),
    }))
}

/// What a `security_and_analysis` update changed.
#[derive(Debug, Clone, Copy, Default)]
pub struct Change {
    pub before: Stored,
    pub after: Stored,
}

impl Change {
    /// Secret scanning (effectively) turned on: scan the history.
    pub fn scanning_enabled(&self) -> bool {
        self.after.secret_scanning && !self.before.secret_scanning
    }

    pub fn changed(&self) -> bool {
        self.before != self.after
    }
}

/// Parse `{"secret_scanning": {"status": "enabled"}, ...}`: `None` for
/// fields not given; 422 for bad values.
fn parse_status(v: &Value, key: &str) -> ApiResult<Option<bool>> {
    let Some(f) = v.get(key) else {
        return Ok(None);
    };
    match f.get("status").and_then(Value::as_str) {
        Some("enabled") => Ok(Some(true)),
        Some("disabled") => Ok(Some(false)),
        _ => Err(ApiError::invalid_field(FieldError::invalid(
            "Repository",
            &format!("security_and_analysis.{key}"),
        ))),
    }
}

/// Apply a `PATCH /repos/{o}/{r}` `security_and_analysis` object inside the
/// caller's transaction (the caller audits and enqueues the history scan
/// via [`crate::jobs::enqueue_history_scan`] when [`Change::scanning_enabled`]).
pub async fn apply(conn: &mut sqlx::PgConnection, repo_id: i64, v: &Value) -> ApiResult<Change> {
    if !v.is_object() {
        return Err(ApiError::invalid_field(FieldError::invalid(
            "Repository",
            "security_and_analysis",
        )));
    }
    let before = Stored::load(&mut *conn, repo_id).await?;
    let mut after = before;
    if let Some(b) = parse_status(v, "secret_scanning")? {
        after.secret_scanning = b;
    }
    if let Some(b) = parse_status(v, "secret_scanning_push_protection")? {
        after.push_protection = b;
    }
    if let Some(b) = parse_status(v, "secret_scanning_non_provider_patterns")? {
        after.non_provider_patterns = b;
    }
    // Accepted but not modelled: advanced_security,
    // secret_scanning_validity_checks, dependabot_security_updates, ...
    for key in ["advanced_security", "secret_scanning_validity_checks"] {
        parse_status(v, key)?;
    }
    if after != before {
        sqlx::query(
            "INSERT INTO repo_security_settings
                 (repo_id, secret_scanning, push_protection, non_provider_patterns)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (repo_id) DO UPDATE SET
                 secret_scanning = EXCLUDED.secret_scanning,
                 push_protection = EXCLUDED.push_protection,
                 non_provider_patterns = EXCLUDED.non_provider_patterns,
                 updated_at = now()",
        )
        .bind(repo_id)
        .bind(after.secret_scanning)
        .bind(after.push_protection)
        .bind(after.non_provider_patterns)
        .execute(&mut *conn)
        .await?;
    }
    Ok(Change { before, after })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bgh_core::settings::SecretScanningSettings;

    #[test]
    fn push_protection_needs_scanning() {
        let site = SecretScanningSettings::default();
        let e = Effective::compute(
            &site,
            Stored {
                push_protection: true,
                ..Default::default()
            },
        );
        assert!(!e.push_protection);
        let forced = SecretScanningSettings {
            enable_all: true,
            push_protection_all: true,
            ..Default::default()
        };
        let e = Effective::compute(&forced, Stored::default());
        assert!(e.secret_scanning && e.push_protection && e.forced_scanning);
        let off = SecretScanningSettings {
            available: false,
            enable_all: true,
            ..Default::default()
        };
        assert!(!Effective::compute(&off, Stored::default()).secret_scanning);
    }
}
