//! Persisting scan results: alerts deduplicated by (secret type, secret
//! hash) per repository, their locations, push protection bypasses, and
//! the `secret_scanning_alert(_location)` events.

use std::collections::HashMap;

use bgh_core::ApiResult;
use bgh_core::db::Tx;
use bgh_core::events::Event;
use bgh_core::state::AppState;

use crate::patterns::{Engine, Kind, secret_hash};
use crate::scan::Hit;

/// Advisory lock key namespace of per-repository alert numbering.
const LOCK_NS: i64 = 0x7700 << 48;

#[derive(Debug, Default, Clone, Copy)]
pub struct Recorded {
    pub alerts_created: usize,
    pub locations_created: usize,
}

/// Store `hits` (from `engine`) for `repo_id`. New alerts of secrets whose
/// push was allowed by a bypass record the bypass; "used in tests" and
/// "false positive" bypasses close them with that resolution.
pub async fn record(
    state: &AppState,
    repo_id: i64,
    engine: &Engine,
    hits: &[Hit],
) -> ApiResult<Recorded> {
    let mut rec = Recorded::default();
    if hits.is_empty() {
        return Ok(rec);
    }
    // Group by alert key, keeping hit order (oldest commits last in
    // rev-list order; the first location is the first stored).
    let mut keys: Vec<(usize, String, String)> = Vec::new(); // (pattern, secret, hash)
    let mut by_key: HashMap<(String, String), Vec<&Hit>> = HashMap::new();
    for h in hits {
        let p = &engine.patterns[h.finding.pattern];
        let hash = secret_hash(&h.finding.secret);
        let k = (p.secret_type.clone(), hash.clone());
        let e = by_key.entry(k).or_default();
        if e.is_empty() {
            keys.push((h.finding.pattern, h.finding.secret.clone(), hash));
        }
        e.push(h);
    }

    let mut tx = Tx::begin(state).await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(LOCK_NS | repo_id)
        .execute(&mut *tx)
        .await?;
    for (pattern, secret, hash) in keys {
        let p = &engine.patterns[pattern];
        let custom_id = match p.kind {
            Kind::Custom(id) => Some(id),
            _ => None,
        };
        let sealed = bgh_core::secretbox::seal(state, &secret)?;
        let inserted: Option<i64> = sqlx::query_scalar(
            "INSERT INTO secret_scanning_alerts
                 (repo_id, number, secret_type, secret_type_display_name, secret_hash,
                  secret_sealed, custom_pattern_id)
             VALUES ($1, (SELECT coalesce(max(number), 0) + 1 FROM secret_scanning_alerts
                           WHERE repo_id = $1), $2, $3, $4, $5, $6)
             ON CONFLICT (repo_id, secret_type, secret_hash) DO NOTHING
             RETURNING id",
        )
        .bind(repo_id)
        .bind(&p.secret_type)
        .bind(&p.display_name)
        .bind(&hash)
        .bind(&sealed)
        .bind(custom_id)
        .fetch_optional(&mut *tx)
        .await?;
        let (alert_id, created) = match inserted {
            Some(id) => (id, true),
            None => (
                sqlx::query_scalar(
                    "SELECT id FROM secret_scanning_alerts
                      WHERE repo_id = $1 AND secret_type = $2 AND secret_hash = $3",
                )
                .bind(repo_id)
                .bind(&p.secret_type)
                .bind(&hash)
                .fetch_one(&mut *tx)
                .await?,
                false,
            ),
        };
        if created {
            rec.alerts_created += 1;
            apply_bypass(&mut tx, repo_id, alert_id, &hash).await?;
            tx.emit(Event::SecretScanningAlert {
                repo_id,
                alert_id,
                action: "created".into(),
                actor_id: None,
            });
        }
        let key = (p.secret_type.clone(), hash);
        for h in by_key.get(&key).map(Vec::as_slice).unwrap_or_default() {
            let f = &h.finding;
            let loc: Option<i64> = sqlx::query_scalar(
                "INSERT INTO secret_scanning_locations
                     (alert_id, commit_sha, path, blob_sha, start_line, end_line,
                      start_column, end_column)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 ON CONFLICT (alert_id, commit_sha, path, start_line, start_column) DO NOTHING
                 RETURNING id",
            )
            .bind(alert_id)
            .bind(&h.file.commit)
            .bind(&h.file.path)
            .bind(&h.file.blob)
            .bind(f.start_line)
            .bind(f.end_line)
            .bind(f.start_column)
            .bind(f.end_column)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(location_id) = loc {
                rec.locations_created += 1;
                if !created {
                    tx.emit(Event::SecretScanningAlertLocationCreated {
                        repo_id,
                        alert_id,
                        location_id,
                    });
                }
            }
        }
    }
    tx.commit().await?;
    Ok(rec)
}

/// Mark a new alert as pushed through a bypass (most recent bypass of the
/// same secret in the repository).
async fn apply_bypass(tx: &mut Tx, repo_id: i64, alert_id: i64, hash: &str) -> ApiResult<()> {
    let bypass: Option<(Option<i64>, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT user_id, reason, bypassed_at FROM secret_scanning_push_blocks
          WHERE repo_id = $1 AND secret_hash = $2 AND bypassed_at IS NOT NULL
          ORDER BY bypassed_at DESC, id DESC LIMIT 1",
    )
    .bind(repo_id)
    .bind(hash)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((user_id, reason, at)) = bypass else {
        return Ok(());
    };
    let resolution = match reason.as_str() {
        "used_in_tests" => Some("used_in_tests"),
        "false_positive" => Some("false_positive"),
        _ => None,
    };
    sqlx::query(
        "UPDATE secret_scanning_alerts SET
             push_protection_bypassed = true,
             push_protection_bypassed_by_id = $2,
             push_protection_bypassed_at = $3,
             state = CASE WHEN $4::text IS NULL THEN state ELSE 'resolved' END,
             resolution = $4,
             resolved_by_id = CASE WHEN $4::text IS NULL THEN NULL ELSE $2 END,
             resolved_at = CASE WHEN $4::text IS NULL THEN NULL ELSE now() END
         WHERE id = $1",
    )
    .bind(alert_id)
    .bind(user_id)
    .bind(at)
    .bind(resolution)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
