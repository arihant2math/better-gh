//! Branch protection requirements for merging a pull request: required
//! reviews (approval count, code owners, changes requested), required
//! status checks (commit statuses and check runs, strict = up to date),
//! conversation resolution and linear history.
//!
//! Rules are read from `branch_protections` (managed by bgh-repos); every
//! rule whose pattern matches the base branch applies (strictest wins).

use std::collections::{HashMap, HashSet};

use bgh_core::prelude::*;
use serde_json::Value;

use crate::codeowners;
use crate::git;
use crate::model::Pull;

#[derive(Debug, Clone, Default)]
pub struct ReviewRules {
    pub required_approving_review_count: i64,
    pub require_code_owner_reviews: bool,
    pub dismiss_stale_reviews: bool,
    pub require_last_push_approval: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CheckRules {
    pub strict: bool,
    pub contexts: Vec<String>,
}

/// Combined protection rules for a branch.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    pub protected: bool,
    pub reviews: Option<ReviewRules>,
    pub checks: Option<CheckRules>,
    pub conversation_resolution: bool,
    pub linear_history: bool,
    pub enforce_admins: bool,
    pub allow_deletions: bool,
    pub lock_branch: bool,
}

#[derive(sqlx::FromRow)]
struct RuleRow {
    pattern: String,
    required_status_checks: Option<Value>,
    required_pull_request_reviews: Option<Value>,
    enforce_admins: bool,
    required_linear_history: bool,
    required_conversation_resolution: bool,
    allow_deletions: bool,
    lock_branch: bool,
}

pub async fn rules_for(db: &sqlx::PgPool, repo_id: i64, branch: &str) -> ApiResult<Rules> {
    let rows: Vec<RuleRow> = sqlx::query_as(
        "SELECT pattern, required_status_checks, required_pull_request_reviews, enforce_admins,
                required_linear_history, required_conversation_resolution, allow_deletions,
                lock_branch
           FROM branch_protections WHERE repo_id = $1",
    )
    .bind(repo_id)
    .fetch_all(db)
    .await?;
    let mut rules = Rules {
        allow_deletions: true,
        ..Rules::default()
    };
    for r in rows
        .into_iter()
        .filter(|r| bgh_repos::protection::pattern_matches(&r.pattern, branch))
    {
        rules.protected = true;
        rules.enforce_admins |= r.enforce_admins;
        rules.linear_history |= r.required_linear_history;
        rules.conversation_resolution |= r.required_conversation_resolution;
        rules.allow_deletions &= r.allow_deletions;
        rules.lock_branch |= r.lock_branch;
        if let Some(v) = r.required_pull_request_reviews.filter(|v| v.is_object()) {
            let cur = rules.reviews.get_or_insert_with(ReviewRules::default);
            let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
            cur.required_approving_review_count = cur.required_approving_review_count.max(
                v.get("required_approving_review_count")
                    .and_then(Value::as_i64)
                    .unwrap_or(1),
            );
            cur.require_code_owner_reviews |= b("require_code_owner_reviews");
            cur.dismiss_stale_reviews |= b("dismiss_stale_reviews");
            cur.require_last_push_approval |= b("require_last_push_approval");
        }
        if let Some(v) = r.required_status_checks.filter(|v| v.is_object()) {
            let cur = rules.checks.get_or_insert_with(CheckRules::default);
            cur.strict |= v.get("strict").and_then(Value::as_bool).unwrap_or(false);
            let mut add = |c: &str| {
                if !cur.contexts.iter().any(|x| x == c) {
                    cur.contexts.push(c.to_string());
                }
            };
            for c in v
                .get("contexts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(c) = c.as_str() {
                    add(c);
                }
            }
            for c in v
                .get("checks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(c) = c.get("context").and_then(Value::as_str) {
                    add(c);
                }
            }
        }
    }
    Ok(rules)
}

/// State of one status context / check name on a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    Success,
    Pending,
    Failure,
}

/// Latest outcome per status context and per check run name for `sha`
/// (in either the base or the head repository).
pub async fn check_outcomes(
    db: &sqlx::PgPool,
    repo_ids: &[i64],
    sha: &str,
) -> ApiResult<HashMap<String, CheckOutcome>> {
    let statuses: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (context) context, state FROM commit_statuses
          WHERE repo_id = ANY($1) AND sha = $2 ORDER BY context, id DESC",
    )
    .bind(repo_ids)
    .bind(sha)
    .fetch_all(db)
    .await?;
    let runs: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT ON (name) name, status, conclusion FROM check_runs
          WHERE repo_id = ANY($1) AND head_sha = $2 ORDER BY name, id DESC",
    )
    .bind(repo_ids)
    .bind(sha)
    .fetch_all(db)
    .await?;
    let mut out = HashMap::new();
    for (ctx, st) in statuses {
        out.insert(
            ctx,
            match st.as_str() {
                "success" => CheckOutcome::Success,
                "pending" => CheckOutcome::Pending,
                _ => CheckOutcome::Failure,
            },
        );
    }
    for (name, status, conclusion) in runs {
        let o = if status != "completed" {
            CheckOutcome::Pending
        } else {
            match conclusion.as_deref() {
                Some("success" | "neutral" | "skipped") => CheckOutcome::Success,
                _ => CheckOutcome::Failure,
            }
        };
        // A check run and a status with the same name: the worse wins.
        out.entry(name)
            .and_modify(|cur| {
                if o == CheckOutcome::Failure
                    || (o == CheckOutcome::Pending && *cur == CheckOutcome::Success)
                {
                    *cur = o;
                }
            })
            .or_insert(o);
    }
    Ok(out)
}

/// Result of evaluating merge requirements.
#[derive(Debug, Clone, Default)]
pub struct Evaluation {
    /// Unmet requirements (GitHub-style messages); non-empty ⇒ `blocked`.
    pub blockers: Vec<String>,
    /// Strict status checks and the head is not up to date with the base.
    pub behind: bool,
    /// Non-required checks failing or pending.
    pub unstable: bool,
    pub approvals: i64,
    pub changes_requested: bool,
}

/// Latest decisive review state per reviewer (APPROVED / CHANGES_REQUESTED;
/// a later DISMISSED review clears it).
pub async fn review_states(db: &sqlx::PgPool, pull_id: i64) -> ApiResult<Vec<(i64, String)>> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (user_id) user_id, state FROM pr_reviews
          WHERE pull_id = $1 AND user_id IS NOT NULL
            AND state IN ('APPROVED', 'CHANGES_REQUESTED', 'DISMISSED')
          ORDER BY user_id, submitted_at DESC NULLS LAST, id DESC",
    )
    .bind(pull_id)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().filter(|(_, s)| s != "DISMISSED").collect())
}

pub async fn evaluate(
    state: &AppState,
    repo: &db::Repository,
    pull: &Pull,
    rules: &Rules,
) -> ApiResult<Evaluation> {
    let mut ev = Evaluation::default();
    let db = &state.db;

    // Reviews.
    let reviews = review_states(db, pull.id()).await?;
    let mut approvers: HashSet<i64> = HashSet::new();
    for (uid, st) in &reviews {
        if Some(*uid) == pull.issue.author_id {
            continue;
        }
        let perm = crate::pulls::member_permission(state, repo, *uid).await?;
        if perm < Permission::Write {
            continue;
        }
        match st.as_str() {
            "APPROVED" => {
                approvers.insert(*uid);
            }
            "CHANGES_REQUESTED" => ev.changes_requested = true,
            _ => {}
        }
    }
    ev.approvals = approvers.len() as i64;
    if let Some(r) = &rules.reviews {
        if ev.changes_requested {
            ev.blockers
                .push("Changes requested by a reviewer with write access.".into());
        }
        if ev.approvals < r.required_approving_review_count {
            let n = r.required_approving_review_count;
            ev.blockers.push(format!(
                "At least {n} approving review{} is required by reviewers with write access.",
                if n == 1 { "" } else { "s" }
            ));
        }
        if r.require_code_owner_reviews {
            let missing =
                codeowners::missing_owner_approvals(state, repo, pull, &approvers).await?;
            if !missing.is_empty() {
                ev.blockers.push(format!(
                    "Waiting on code owner review from {}.",
                    missing.join(", ")
                ));
            }
        }
    }

    // Status checks.
    let mut repo_ids = vec![pull.pr.repo_id];
    if let Some(h) = pull.pr.head_repo_id
        && h != pull.pr.repo_id
    {
        repo_ids.push(h);
    }
    let outcomes = check_outcomes(db, &repo_ids, &pull.pr.head_sha).await?;
    let required: Vec<String> = rules
        .checks
        .as_ref()
        .map(|c| c.contexts.clone())
        .unwrap_or_default();
    for ctx in &required {
        match outcomes.get(ctx) {
            None => ev
                .blockers
                .push(format!("Required status check \"{ctx}\" is expected.")),
            Some(CheckOutcome::Pending) => ev
                .blockers
                .push(format!("Required status check \"{ctx}\" is in progress.")),
            Some(CheckOutcome::Failure) => ev
                .blockers
                .push(format!("Required status check \"{ctx}\" is failing.")),
            Some(CheckOutcome::Success) => {}
        }
    }
    ev.unstable = outcomes
        .iter()
        .any(|(k, o)| *o != CheckOutcome::Success && !required.contains(k));
    if let Some(c) = &rules.checks
        && c.strict
    {
        let store = git::store(state);
        ev.behind = !bgh_git::merge::is_ancestor(
            &store,
            pull.pr.repo_id,
            &pull.pr.base_sha,
            &pull.pr.head_sha,
        )
        .await?;
        if ev.behind {
            ev.blockers
                .push("Head branch is out of date with the base branch.".into());
        }
    }

    // Conversations.
    if rules.conversation_resolution {
        let unresolved: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pr_review_comments c
              LEFT JOIN pr_reviews r ON r.id = c.review_id
              WHERE c.pull_id = $1 AND c.in_reply_to_id IS NULL AND c.resolved_at IS NULL
                AND (r.id IS NULL OR r.state <> 'PENDING')",
        )
        .bind(pull.id())
        .fetch_one(db)
        .await?;
        if unresolved > 0 {
            ev.blockers.push("All comments must be resolved.".into());
        }
    }
    Ok(ev)
}

/// `mergeable_state` from the pieces (GitHub precedence).
pub fn mergeable_state(draft: bool, mergeable: Option<bool>, ev: &Evaluation) -> &'static str {
    if draft {
        "draft"
    } else if mergeable == Some(false) {
        "dirty"
    } else if mergeable.is_none() {
        "unknown"
    } else if ev.behind {
        "behind"
    } else if !ev.blockers.is_empty() {
        "blocked"
    } else if ev.unstable {
        "unstable"
    } else {
        "clean"
    }
}
