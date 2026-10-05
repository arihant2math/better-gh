//! Push protection: the receive-pack object check that scans the blobs a
//! push adds (still quarantined) and rejects the push with GitHub's GH013
//! report and an unblock URL per secret. A bypass through that URL lets
//! the pusher push the same secret for [`crate::alerts::BYPASS_TTL_HOURS`].
//!
//! Wiring (bgh-repos, HTTP and SSH): [`prepare`] before reading the pack,
//! then [`combine`] the [`PushProtection::object_check`] of the ref updates
//! into `PushPolicy::object_check`. The check is additive to other object
//! checks (ruleset push rules) and bounded: only new blobs up to the
//! site's `max_blob_kb`, within `push_scan_timeout_secs` (fail open).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use bgh_core::events::{RefUpdate, ZERO_SHA};
use bgh_core::prelude::*;
use bgh_git::smart_http::{HookVerdict, ObjectCheck, QuarantineEnv};

use crate::patterns::{Engine, secret_hash};
use crate::scan;
use crate::settings::Effective;

/// Push protection context of one push (cheap to clone).
#[derive(Clone)]
pub struct PushProtection {
    inner: Arc<Inner>,
}

struct Inner {
    state: AppState,
    repo_id: i64,
    owner: String,
    repo: String,
    pusher_id: Option<i64>,
    engine: Arc<Engine>,
    eff: Effective,
}

/// Page where a blocked pusher allows a secret.
pub fn unblock_url(state: &AppState, owner: &str, repo: &str, placeholder: &str) -> String {
    state.urls.html(&format!(
        "/{owner}/{repo}/security/secret-scanning/unblock-secret/{placeholder}"
    ))
}

/// Push protection for a push to `repo` by `pusher_id` (`None` for deploy
/// keys), or `None` when it is off.
pub async fn prepare(
    state: &AppState,
    repo: &db::Repository,
    owner_login: &str,
    pusher_id: Option<i64>,
) -> ApiResult<Option<PushProtection>> {
    let eff = crate::settings::effective(state, repo.id).await?;
    if !eff.push_protection {
        return Ok(None);
    }
    let custom = crate::custom::patterns_for_repo(&state.db, repo.id).await?;
    let engine = Engine::for_repo(false, custom).push_protected();
    if engine.is_empty() {
        return Ok(None);
    }
    Ok(Some(PushProtection {
        inner: Arc::new(Inner {
            state: state.clone(),
            repo_id: repo.id,
            owner: owner_login.to_string(),
            repo: repo.name.clone(),
            pusher_id,
            engine,
            eff,
        }),
    }))
}

impl PushProtection {
    /// The object check for these ref updates.
    pub fn object_check(&self, updates: &[RefUpdate]) -> ObjectCheck {
        let this = self.clone();
        let updates = updates.to_vec();
        ObjectCheck::new(move |env: QuarantineEnv| {
            let this = this.clone();
            let updates = updates.clone();
            async move {
                match this.check(&env, &updates).await {
                    Ok(v) => v,
                    Err(e) => {
                        // Fail open like GitHub: the background scan still
                        // reports whatever got in.
                        tracing::warn!(
                            repo = this.inner.repo_id,
                            "push protection scan failed: {e:#}"
                        );
                        HookVerdict {
                            accept: true,
                            lines: vec![],
                        }
                    }
                }
            }
        })
    }

    async fn check(
        &self,
        env: &QuarantineEnv,
        updates: &[RefUpdate],
    ) -> anyhow::Result<HookVerdict> {
        let i = &self.inner;
        let mut news: Vec<&str> = updates
            .iter()
            .filter(|u| u.new != ZERO_SHA && bgh_git::is_sha(&u.new))
            .map(|u| u.new.as_str())
            .collect();
        news.sort_unstable();
        news.dedup();
        if news.is_empty() {
            return Ok(HookVerdict {
                accept: true,
                lines: vec![],
            });
        }
        let started = Instant::now();
        let deadline = started + i.eff.push_timeout;
        let store = bgh_git::RepoStore::from_config(&i.state.config);
        let cli = store.cli(i.repo_id)?;
        let envs = env.envs();
        // Refs haven't moved yet: `--all` is everything already accepted.
        let mut args = news.clone();
        args.extend(["--not", "--all"]);
        let commits = scan::rev_list(&cli, &args, &envs).await?;
        let out = scan::scan_commits(
            &cli,
            &commits,
            &envs,
            i.engine.clone(),
            i.eff.max_blob,
            Some(deadline),
        )
        .await?;
        tracing::debug!(
            repo = i.repo_id,
            blobs = out.blobs_scanned,
            bytes = out.bytes_scanned,
            ms = started.elapsed().as_millis() as u64,
            "push protection scan"
        );
        if out.timed_out {
            tracing::warn!(
                repo = i.repo_id,
                "push protection scan timed out; push accepted"
            );
            return Ok(HookVerdict {
                accept: true,
                lines: vec![
                    "warning: secret scanning did not finish in time; this push was not checked for secrets."
                        .into(),
                ],
            });
        }
        if out.hits.is_empty() {
            return Ok(HookVerdict {
                accept: true,
                lines: vec![],
            });
        }

        // One entry per distinct secret, with its locations (first first).
        let mut secrets: BTreeMap<(String, String), Vec<&scan::Hit>> = BTreeMap::new();
        let mut order: Vec<(String, String)> = Vec::new();
        for h in &out.hits {
            let key = (
                i.engine.patterns[h.finding.pattern].secret_type.clone(),
                secret_hash(&h.finding.secret),
            );
            let e = secrets.entry(key.clone()).or_default();
            if e.is_empty() {
                order.push(key);
            }
            e.push(h);
        }
        let hashes: Vec<String> = order.iter().map(|(_, h)| h.clone()).collect();
        // Allowed: bypassed by this pusher and not expired, or already
        // dismissed as harmless in an alert of this repository.
        let allowed: HashSet<String> = sqlx::query_scalar(
            "SELECT secret_hash FROM secret_scanning_push_blocks
              WHERE repo_id = $1 AND secret_hash = ANY($2)
                AND user_id IS NOT DISTINCT FROM $3
                AND bypassed_at IS NOT NULL AND expires_at > now()
             UNION
             SELECT secret_hash FROM secret_scanning_alerts
              WHERE repo_id = $1 AND secret_hash = ANY($2)
                AND resolution IN ('false_positive', 'used_in_tests', 'wont_fix')",
        )
        .bind(i.repo_id)
        .bind(&hashes)
        .bind(i.pusher_id)
        .fetch_all(&i.state.db)
        .await?
        .into_iter()
        .collect();
        let blocked: Vec<&(String, String)> =
            order.iter().filter(|(_, h)| !allowed.contains(h)).collect();
        if blocked.is_empty() {
            return Ok(HookVerdict {
                accept: true,
                lines: vec![],
            });
        }

        let refname = updates
            .iter()
            .find(|u| u.new != ZERO_SHA)
            .map(|u| u.refname.as_str())
            .unwrap_or("HEAD");
        let mut lines = vec![
            format!("error: GH013: Repository rule violations found for {refname}."),
            String::new(),
            "- GITHUB PUSH PROTECTION".into(),
            "  —————————————————————————————————————————".into(),
            "    Resolve the following violations before pushing again".into(),
            String::new(),
            "    - Push cannot contain secrets".into(),
            String::new(),
        ];
        const MAX_LISTED: usize = 10;
        let mut tx = i.state.db.begin().await?;
        for key in blocked.iter().take(MAX_LISTED) {
            let hits = &secrets[*key];
            let first = hits[0];
            let p = &i.engine.patterns[first.finding.pattern];
            let placeholder = bgh_core::crypto::random_token(26);
            let preview: String = first.finding.secret.chars().take(4).collect();
            sqlx::query(
                "INSERT INTO secret_scanning_push_blocks
                     (placeholder_id, repo_id, user_id, secret_type, secret_type_display_name,
                      secret_hash, secret_preview, commit_sha, path, start_line)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(&placeholder)
            .bind(i.repo_id)
            .bind(i.pusher_id)
            .bind(&p.secret_type)
            .bind(&p.display_name)
            .bind(&key.1)
            .bind(&preview)
            .bind(&first.file.commit)
            .bind(&first.file.path)
            .bind(first.finding.start_line)
            .execute(&mut *tx)
            .await?;
            lines.push(String::new());
            lines.push(format!(
                "      —— {} ——————————————————————————",
                p.display_name
            ));
            lines.push("       locations:".into());
            for h in hits.iter().take(5) {
                lines.push(format!(
                    "         - commit: {}",
                    &h.file.commit[..h.file.commit.len().min(9)]
                ));
                lines.push(format!(
                    "           path: {}:{}",
                    h.file.path, h.finding.start_line
                ));
            }
            if hits.len() > 5 {
                lines.push(format!("         ... and {} more", hits.len() - 5));
            }
            lines.push(String::new());
            lines.push(
                "       (?) To push, remove secret from commit(s) or follow this URL to allow the secret."
                    .into(),
            );
            lines.push(format!(
                "       {}",
                unblock_url(&i.state, &i.owner, &i.repo, &placeholder)
            ));
        }
        if blocked.len() > MAX_LISTED {
            lines.push(String::new());
            lines.push(format!(
                "      ... and {} more secrets; remove the listed ones and push again to see them.",
                blocked.len() - MAX_LISTED
            ));
        }
        lines.push(String::new());
        tx.commit().await?;
        Ok(HookVerdict {
            accept: false,
            lines,
        })
    }
}

/// Run both object checks; the push is accepted only if both accept, and
/// both checks' messages are shown.
pub fn combine(a: Option<ObjectCheck>, b: Option<ObjectCheck>) -> Option<ObjectCheck> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => Some(ObjectCheck::new(move |env: QuarantineEnv| {
            let (a, b) = (a.clone(), b.clone());
            async move {
                let (va, vb) = futures::join!((a.0)(env.clone()), (b.0)(env));
                let mut lines = va.lines;
                lines.extend(vb.lines);
                HookVerdict {
                    accept: va.accept && vb.accept,
                    lines,
                }
            }
        })),
    }
}
