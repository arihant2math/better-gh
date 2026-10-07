//! P51 GitHub steps `hooks`, `branch_protection`, `rulesets`: repository
//! configuration. These endpoints need an admin token on the source; a
//! 403/404 skips the step (logged) instead of failing the import.
//!
//! * Webhooks are imported **disabled**: GitHub never returns their secret,
//!   and the target has to be reviewed before deliveries start.
//! * Branch protection: one rule per protected branch (users/teams of
//!   push restrictions mapped; unmapped ones dropped, logged).
//! * Rulesets: branch/tag rulesets with conditions and rules; bypass
//!   actors refer to source ids and are dropped (logged).

use bgh_core::prelude::*;
use serde_json::{Value, json};

use crate::client::HttpError;
use crate::pipeline::{Ctx, Item};
use crate::row;

fn forbidden(e: &anyhow::Error) -> bool {
    e.downcast_ref::<HttpError>()
        .is_some_and(|h| matches!(h.status, 403 | 404))
}

impl Ctx {
    /// Run a config step, skipping it when the token can't read it.
    async fn admin_only(&mut self, what: &str, first: String, kind: Item) -> anyhow::Result<()> {
        match self.paged(first, kind).await {
            Err(e) if forbidden(&e) => {
                row::log(
                    &self.state.db,
                    self.row.id,
                    "warn",
                    &format!(
                        "{what}: not readable with this token (needs admin on the source), skipped"
                    ),
                )
                .await;
                Ok(())
            }
            other => other,
        }
    }

    pub(crate) async fn hooks(&mut self) -> anyhow::Result<()> {
        let first = format!("/repos/{}/hooks?per_page=100", self.src());
        self.admin_only("webhooks", first, Item::Hook).await
    }

    pub(crate) async fn hook(&mut self, h: Value) -> anyhow::Result<()> {
        let Some(source_id) = h["id"].as_i64() else {
            return Ok(());
        };
        let Some(url) = h["config"]["url"].as_str().filter(|u| !u.is_empty()) else {
            return Ok(());
        };
        if self.mapped("hook", &source_id.to_string()).await?.is_some() {
            return Ok(());
        }
        let events: Vec<String> = h["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_str().map(str::to_string))
            .collect();
        let insecure = matches!(h["config"]["insecure_ssl"].as_str(), Some("1"))
            || h["config"]["insecure_ssl"].as_i64() == Some(1);
        let content_type = match h["config"]["content_type"].as_str() {
            Some("json") => "json",
            _ => "form",
        };
        let repo_id = self.repo_id()?;
        let mut tx = Tx::begin(&self.state).await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO webhooks (repo_id, name, url, content_type, insecure_ssl, events, active,
                                   creator_id)
             VALUES ($1, 'web', $2, $3, $4, $5, false, $6) RETURNING id",
        )
        .bind(repo_id)
        .bind(url)
        .bind(content_type)
        .bind(insecure)
        .bind(if events.is_empty() {
            vec!["push".to_string()]
        } else {
            events
        })
        .bind(self.row.created_by)
        .fetch_one(&mut *tx)
        .await?;
        self.map(&mut tx, "hook", &source_id.to_string(), id)
            .await?;
        row::bump(&mut tx, self.row.id, "hooks", 1).await?;
        tx.commit().await?;
        row::log(
            &self.state.db,
            self.row.id,
            "info",
            &format!("webhook {url}: imported disabled (set its secret, then activate it)"),
        )
        .await;
        self.tick().await
    }

    pub(crate) async fn branch_protection(&mut self) -> anyhow::Result<()> {
        let first = format!("/repos/{}/branches?protected=true&per_page=100", self.src());
        self.admin_only("branch protection", first, Item::ProtectedBranch)
            .await
    }

    pub(crate) async fn protected_branch(&mut self, b: Value) -> anyhow::Result<()> {
        let Some(name) = b["name"].as_str() else {
            return Ok(());
        };
        if self.mapped("protection", name).await?.is_some() {
            return Ok(());
        }
        let p = match self
            .gh
            .get(&format!(
                "/repos/{}/branches/{}/protection",
                self.src(),
                bgh_core::urls::encode_path(name)
            ))
            .await
        {
            Ok((p, _)) => p,
            Err(e) if forbidden(&e) => return Ok(()),
            Err(e) => return Err(e),
        };
        let on = |key: &str| p[key]["enabled"].as_bool().unwrap_or(false);
        let checks = p
            .get("required_status_checks")
            .filter(|v| !v.is_null())
            .map(|c| {
                json!({
                    "strict": c["strict"].as_bool().unwrap_or(false),
                    "contexts": c["contexts"].as_array().cloned().unwrap_or_default(),
                    "checks": c["checks"].as_array().cloned().unwrap_or_default()
                        .into_iter()
                        .map(|k| json!({"context": k["context"], "app_id": Value::Null}))
                        .collect::<Vec<_>>(),
                })
            });
        let reviews = p
            .get("required_pull_request_reviews")
            .filter(|v| !v.is_null())
            .map(|r| {
                json!({
                    "dismiss_stale_reviews": r["dismiss_stale_reviews"].as_bool().unwrap_or(false),
                    "require_code_owner_reviews": r["require_code_owner_reviews"].as_bool().unwrap_or(false),
                    "required_approving_review_count": r["required_approving_review_count"].as_i64().unwrap_or(1),
                    "require_last_push_approval": r["require_last_push_approval"].as_bool().unwrap_or(false),
                })
            });
        let restrictions = match p.get("restrictions").filter(|v| !v.is_null()) {
            Some(r) => {
                let mut users = Vec::new();
                for u in r["users"].as_array().into_iter().flatten() {
                    if let Some(id) = self.users.resolve(&self.state, &self.gh, u).await? {
                        users.push(id);
                    }
                }
                let mut teams = Vec::new();
                for t in r["teams"].as_array().into_iter().flatten() {
                    if let Some(id) = self.local_team(t["slug"].as_str().unwrap_or("")).await? {
                        teams.push(id);
                    }
                }
                Some(json!({"users": users, "teams": teams, "apps": []}))
            }
            None => None,
        };
        let repo_id = self.repo_id()?;
        let mut tx = Tx::begin(&self.state).await?;
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO branch_protections (repo_id, pattern, required_status_checks,
                    required_pull_request_reviews, restrictions, enforce_admins,
                    required_linear_history, allow_force_pushes, allow_deletions,
                    block_creations, required_conversation_resolution, required_signatures,
                    lock_branch, allow_fork_syncing)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             ON CONFLICT (repo_id, pattern) DO NOTHING RETURNING id",
        )
        .bind(repo_id)
        .bind(name)
        .bind(checks)
        .bind(reviews)
        .bind(restrictions)
        .bind(on("enforce_admins"))
        .bind(on("required_linear_history"))
        .bind(on("allow_force_pushes"))
        .bind(on("allow_deletions"))
        .bind(on("block_creations"))
        .bind(on("required_conversation_resolution"))
        .bind(on("required_signatures"))
        .bind(on("lock_branch"))
        .bind(on("allow_fork_syncing"))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(id) = id else {
            return Ok(());
        };
        self.map(&mut tx, "protection", name, id).await?;
        row::bump(&mut tx, self.row.id, "branch_protections", 1).await?;
        tx.commit().await?;
        self.tick().await
    }

    pub(crate) async fn rulesets(&mut self) -> anyhow::Result<()> {
        let first = format!(
            "/repos/{}/rulesets?includes_parents=false&per_page=100",
            self.src()
        );
        self.admin_only("rulesets", first, Item::Ruleset).await
    }

    pub(crate) async fn ruleset(&mut self, r: Value) -> anyhow::Result<()> {
        let Some(source_id) = r["id"].as_i64() else {
            return Ok(());
        };
        // Organization rulesets show up with `source_type: Organization`.
        if r["source_type"].as_str().is_some_and(|t| t != "Repository")
            || self
                .mapped("ruleset", &source_id.to_string())
                .await?
                .is_some()
        {
            return Ok(());
        }
        let full = match self
            .gh
            .get(&format!("/repos/{}/rulesets/{source_id}", self.src()))
            .await
        {
            Ok((full, _)) => full,
            Err(e) if forbidden(&e) => return Ok(()),
            Err(e) => return Err(e),
        };
        let Some(name) = full["name"].as_str() else {
            return Ok(());
        };
        let target = match full["target"].as_str().unwrap_or("branch") {
            t @ ("branch" | "tag") => t,
            other => {
                row::log(
                    &self.state.db,
                    self.row.id,
                    "warn",
                    &format!("ruleset {name}: target {other} is not supported, skipped"),
                )
                .await;
                return Ok(());
            }
        };
        let enforcement = full["enforcement"]
            .as_str()
            .filter(|e| matches!(*e, "disabled" | "active" | "evaluate"))
            .unwrap_or("active");
        if full["bypass_actors"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            row::log(
                &self.state.db,
                self.row.id,
                "info",
                &format!("ruleset {name}: bypass actors refer to source ids and were dropped"),
            )
            .await;
        }
        let repo_id = self.repo_id()?;
        let mut tx = Tx::begin(&self.state).await?;
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO repo_rulesets (repo_id, name, target, enforcement, conditions, rules,
                                        created_by_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (repo_id, lower(name)) DO NOTHING RETURNING id",
        )
        .bind(repo_id)
        .bind(name)
        .bind(target)
        .bind(enforcement)
        .bind(full.get("conditions").cloned().unwrap_or(json!({})))
        .bind(full.get("rules").cloned().unwrap_or(json!([])))
        .bind(self.row.created_by)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(id) = id else {
            return Ok(());
        };
        self.map(&mut tx, "ruleset", &source_id.to_string(), id)
            .await?;
        row::bump(&mut tx, self.row.id, "rulesets", 1).await?;
        tx.commit().await?;
        self.tick().await
    }
}
