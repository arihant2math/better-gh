//! Merge requirements of a pull request: one effective rule set per (repo,
//! base branch) built from the classic branch protection rule protecting
//! the branch and every **active** ruleset selecting it
//! (`bgh_repos::protection::RepoRules`).
//!
//! Each source keeps its own requirements ([`SourceRules`]) so that every
//! unmet requirement ([`Blocker`]) names where it comes from and can be
//! bypassed per source: classic rules by admins unless `enforce_admins`
//! (review requirements also by `bypass_pull_request_allowances`),
//! rulesets by their bypass actors (`always` or `pull_request`). The
//! merged values ([`Rules::reviews`], [`Rules::checks`], ...) are the most
//! restrictive of all sources.
//!
//! Required status checks only count commit statuses and check runs posted
//! to the **base** repository (a fork owner can't satisfy them), and a
//! required check with an expected source (`app_id` / `integration_id`)
//! only counts check runs of that integration.

use std::collections::{HashMap, HashSet};

use bgh_core::prelude::*;
use bgh_repos::protection::{Actor, ProtectionRow, RepoRules, RulesetRow};
use serde::Serialize;
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

impl ReviewRules {
    fn merge(&mut self, o: &ReviewRules) {
        self.required_approving_review_count = self
            .required_approving_review_count
            .max(o.required_approving_review_count);
        self.require_code_owner_reviews |= o.require_code_owner_reviews;
        self.dismiss_stale_reviews |= o.dismiss_stale_reviews;
        self.require_last_push_approval |= o.require_last_push_approval;
    }
}

/// One required status check. `app_id: None` accepts any source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredCheck {
    pub context: String,
    pub app_id: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct CheckRules {
    pub strict: bool,
    pub checks: Vec<RequiredCheck>,
}

impl CheckRules {
    /// Required context names (deduplicated, in order).
    pub fn contexts(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in &self.checks {
            if !out.contains(&c.context) {
                out.push(c.context.clone());
            }
        }
        out
    }

    fn add(&mut self, context: &str, app_id: Option<i64>) {
        // `-1` means "any source", like absent.
        let app_id = app_id.filter(|a| *a >= 0);
        let c = RequiredCheck {
            context: context.to_string(),
            app_id,
        };
        if !self.checks.contains(&c) {
            self.checks.push(c);
        }
    }
}

/// Where a requirement comes from.
#[derive(Debug, Clone)]
pub enum Source {
    Classic {
        pattern: String,
        enforce_admins: bool,
        bypass_allowances: Option<Value>,
    },
    Ruleset(Box<RulesetRow>),
}

impl Source {
    /// Human-readable source: `branch protection rule "main"` or
    /// `ruleset "Protect main"`.
    pub fn label(&self) -> String {
        match self {
            Source::Classic { pattern, .. } => format!("branch protection rule {pattern:?}"),
            Source::Ruleset(r) => format!("ruleset {:?}", r.name),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Source::Classic { .. } => "branch_protection",
            Source::Ruleset(_) => "ruleset",
        }
    }
}

/// Requirements of one source.
#[derive(Debug, Clone)]
pub struct SourceRules {
    pub source: Source,
    /// A pull request is required (classic reviews / `pull_request` rule).
    pub reviews: Option<ReviewRules>,
    pub checks: Option<CheckRules>,
    pub conversation_resolution: bool,
    pub linear_history: bool,
    /// Stored, not enforced yet (P25). `merge_queue` (P39) plugs in next
    /// to it in [`evaluate_source`].
    pub required_signatures: bool,
    /// Environments whose latest deployment of the head commit must have
    /// succeeded (`required_deployments` rule, classic
    /// `required_deployment_environments`).
    pub required_deployments: Vec<String>,
}

impl SourceRules {
    fn classic(p: &ProtectionRow) -> Self {
        let reviews = p
            .required_pull_request_reviews
            .as_ref()
            .filter(|v| v.is_object())
            .map(|v| {
                let b = |k: &str| v[k].as_bool().unwrap_or(false);
                ReviewRules {
                    required_approving_review_count: v["required_approving_review_count"]
                        .as_i64()
                        .unwrap_or(1),
                    require_code_owner_reviews: b("require_code_owner_reviews"),
                    dismiss_stale_reviews: b("dismiss_stale_reviews"),
                    require_last_push_approval: b("require_last_push_approval"),
                }
            });
        let checks = p
            .required_status_checks
            .as_ref()
            .filter(|v| v.is_object())
            .map(|v| {
                let mut c = CheckRules {
                    strict: v["strict"].as_bool().unwrap_or(false),
                    ..CheckRules::default()
                };
                for x in v["checks"].as_array().into_iter().flatten() {
                    if let Some(ctx) = x["context"].as_str() {
                        c.add(ctx, x["app_id"].as_i64());
                    }
                }
                for x in v["contexts"].as_array().into_iter().flatten() {
                    if let Some(ctx) = x.as_str()
                        && !c.checks.iter().any(|k| k.context == ctx)
                    {
                        c.add(ctx, None);
                    }
                }
                c
            });
        Self {
            source: Source::Classic {
                pattern: p.pattern.clone(),
                enforce_admins: p.enforce_admins,
                bypass_allowances: p
                    .required_pull_request_reviews
                    .as_ref()
                    .map(|v| v["bypass_pull_request_allowances"].clone())
                    .filter(|v| !v.is_null()),
            },
            reviews,
            checks,
            conversation_resolution: p.required_conversation_resolution,
            linear_history: p.required_linear_history,
            required_signatures: p.required_signatures,
            required_deployments: p.required_deployment_environments.clone(),
        }
    }

    fn ruleset(r: &RulesetRow) -> Self {
        let mut conversation_resolution = false;
        let reviews = r.find_rule("pull_request").map(|rule| {
            let p = &rule["parameters"];
            let b = |k: &str| p[k].as_bool().unwrap_or(false);
            conversation_resolution = b("required_review_thread_resolution");
            ReviewRules {
                required_approving_review_count: p["required_approving_review_count"]
                    .as_i64()
                    .unwrap_or(0),
                require_code_owner_reviews: b("require_code_owner_review"),
                dismiss_stale_reviews: b("dismiss_stale_reviews_on_push"),
                require_last_push_approval: b("require_last_push_approval"),
            }
        });
        let checks = r.find_rule("required_status_checks").map(|rule| {
            let p = &rule["parameters"];
            let mut c = CheckRules {
                strict: p["strict_required_status_checks_policy"]
                    .as_bool()
                    .unwrap_or(false),
                ..CheckRules::default()
            };
            for x in p["required_status_checks"].as_array().into_iter().flatten() {
                if let Some(ctx) = x["context"].as_str() {
                    c.add(ctx, x["integration_id"].as_i64());
                }
            }
            c
        });
        Self {
            source: Source::Ruleset(Box::new(r.clone())),
            reviews,
            checks,
            conversation_resolution,
            linear_history: r.find_rule("required_linear_history").is_some(),
            required_signatures: r.find_rule("required_signatures").is_some(),
            required_deployments: r
                .find_rule("required_deployments")
                .and_then(|rule| rule["parameters"]["required_deployment_environments"].as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// Effective protection of a branch.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    /// A classic rule or an active ruleset applies.
    pub protected: bool,
    /// Most restrictive values of all sources.
    pub reviews: Option<ReviewRules>,
    pub checks: Option<CheckRules>,
    pub conversation_resolution: bool,
    pub linear_history: bool,
    /// Classic-only settings.
    pub enforce_admins: bool,
    pub allow_deletions: bool,
    pub lock_branch: bool,
    /// Classic push restrictions (`{"users", "teams", "apps"}`): only
    /// listed actors may merge.
    pub restrictions: Option<Value>,
    /// Classic `dismissal_restrictions`: only listed actors (and admins)
    /// may dismiss reviews.
    pub dismissal_restrictions: Option<Value>,
    pub sources: Vec<SourceRules>,
}

impl Rules {
    fn add(&mut self, s: SourceRules) {
        self.protected = true;
        if let Some(r) = &s.reviews {
            self.reviews
                .get_or_insert_with(ReviewRules::default)
                .merge(r);
        }
        if let Some(c) = &s.checks {
            let cur = self.checks.get_or_insert_with(CheckRules::default);
            cur.strict |= c.strict;
            for k in &c.checks {
                cur.add(&k.context, k.app_id);
            }
        }
        self.conversation_resolution |= s.conversation_resolution;
        self.linear_history |= s.linear_history;
        self.sources.push(s);
    }

    /// Whether `actor` may ignore `b` when merging.
    pub fn bypassed_by(&self, b: &Blocker, actor: &Actor) -> bool {
        match &self.sources[b.source_index].source {
            Source::Classic {
                enforce_admins,
                bypass_allowances,
                ..
            } => {
                (actor.permission >= Permission::Admin && !enforce_admins)
                    || (b.review && actor.is_listed_in(bypass_allowances.as_ref()))
            }
            Source::Ruleset(r) => r.bypass_mode(actor) != "never",
        }
    }

    /// Whether classic push restrictions keep `actor` from merging.
    pub fn restricts(&self, actor: &Actor) -> bool {
        self.restrictions.is_some()
            && !(actor.permission >= Permission::Admin && !self.enforce_admins)
            && !actor.is_listed_in(self.restrictions.as_ref())
    }

    /// Whether `actor` may dismiss reviews on this branch.
    pub fn may_dismiss(&self, actor: &Actor) -> bool {
        actor.permission >= Permission::Admin
            || self.dismissal_restrictions.is_none()
            || actor.is_listed_in(self.dismissal_restrictions.as_ref())
    }
}

/// The effective rules for `branch` from all of a repository's rules.
pub fn effective(all: &RepoRules, branch: &str) -> Rules {
    let mut rules = Rules {
        allow_deletions: true,
        ..Rules::default()
    };
    if let Some(p) = all.protection_for(branch) {
        rules.enforce_admins = p.enforce_admins;
        rules.allow_deletions = p.allow_deletions;
        rules.lock_branch = p.lock_branch;
        rules.restrictions = p.restrictions.clone().filter(|v| v.is_object());
        rules.dismissal_restrictions = p
            .required_pull_request_reviews
            .as_ref()
            .map(|v| v["dismissal_restrictions"].clone())
            .filter(|v| v.is_object());
        rules.add(SourceRules::classic(p));
    }
    let refname = format!("refs/heads/{branch}");
    for r in all.rulesets_for(&refname) {
        rules.add(SourceRules::ruleset(r));
    }
    rules
}

/// Load the effective rules of `repo_id`'s `branch` (two indexed queries
/// plus the repository).
pub async fn rules_for(db: &sqlx::PgPool, repo_id: i64, branch: &str) -> ApiResult<Rules> {
    let Some(repo) = db::Repository::find(db, repo_id).await? else {
        return Ok(Rules {
            allow_deletions: true,
            ..Rules::default()
        });
    };
    let all = RepoRules::load(db, &repo).await?;
    Ok(effective(&all, branch))
}

/// State of one status context / check name on a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    Success,
    Pending,
    Failure,
}

impl CheckOutcome {
    fn worse(self, o: CheckOutcome) -> CheckOutcome {
        use CheckOutcome::*;
        match (self, o) {
            (Failure, _) | (_, Failure) => Failure,
            (Pending, _) | (_, Pending) => Pending,
            _ => Success,
        }
    }
}

/// Latest commit status per context and latest check run per (name, app)
/// on one commit of the base repository.
#[derive(Debug, Clone, Default)]
pub struct CheckOutcomes {
    statuses: HashMap<String, CheckOutcome>,
    runs: HashMap<String, Vec<(i64, CheckOutcome)>>,
}

impl CheckOutcomes {
    /// Outcome of a required check. With an expected `app_id` only that
    /// integration's check runs count; otherwise the worst of the status
    /// and the check runs of every integration.
    pub fn get(&self, context: &str, app_id: Option<i64>) -> Option<CheckOutcome> {
        let runs = self.runs.get(context).into_iter().flatten();
        match app_id {
            Some(app) => runs
                .filter(|(a, _)| *a == app)
                .map(|(_, o)| *o)
                .reduce(CheckOutcome::worse),
            None => self
                .statuses
                .get(context)
                .copied()
                .into_iter()
                .chain(runs.map(|(_, o)| *o))
                .reduce(CheckOutcome::worse),
        }
    }

    /// Combined outcome per name.
    pub fn by_name(&self) -> HashMap<&str, CheckOutcome> {
        let mut out: HashMap<&str, CheckOutcome> = HashMap::new();
        let all = self.statuses.iter().map(|(k, o)| (k.as_str(), *o)).chain(
            self.runs
                .iter()
                .flat_map(|(k, v)| v.iter().map(move |(_, o)| (k.as_str(), *o))),
        );
        for (k, o) in all {
            out.entry(k)
                .and_modify(|cur| *cur = cur.worse(o))
                .or_insert(o);
        }
        out
    }

    pub fn any_pending(&self) -> bool {
        self.by_name().values().any(|o| *o == CheckOutcome::Pending)
    }
}

/// Statuses and check runs posted to `repo_id` (the base repository) for
/// `sha`. Never pass a fork: its owner controls what is posted there.
pub async fn check_outcomes(
    db: &sqlx::PgPool,
    repo_id: i64,
    sha: &str,
) -> ApiResult<CheckOutcomes> {
    let statuses: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT ON (context) context, state FROM commit_statuses
          WHERE repo_id = $1 AND sha = $2 ORDER BY context, id DESC",
    )
    .bind(repo_id)
    .bind(sha)
    .fetch_all(db)
    .await?;
    let runs: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT ON (r.name, s.app_slug) r.name, s.app_slug, r.status, r.conclusion
           FROM check_runs r JOIN check_suites s ON s.id = r.check_suite_id
          WHERE r.repo_id = $1 AND r.head_sha = $2 ORDER BY r.name, s.app_slug, r.id DESC",
    )
    .bind(repo_id)
    .bind(sha)
    .fetch_all(db)
    .await?;
    let mut out = CheckOutcomes::default();
    for (ctx, st) in statuses {
        let o = match st.as_str() {
            "success" => CheckOutcome::Success,
            "pending" => CheckOutcome::Pending,
            _ => CheckOutcome::Failure,
        };
        out.statuses.insert(ctx, o);
    }
    for (name, slug, status, conclusion) in runs {
        let o = if status != "completed" {
            CheckOutcome::Pending
        } else {
            match conclusion.as_deref() {
                Some("success" | "neutral" | "skipped") => CheckOutcome::Success,
                _ => CheckOutcome::Failure,
            }
        };
        out.runs
            .entry(name)
            .or_default()
            .push((crate::checks::app_id_for_slug(&slug), o));
    }
    Ok(out)
}

/// One unmet requirement.
#[derive(Debug, Clone, Serialize)]
pub struct Blocker {
    pub message: String,
    /// [`Source::label`].
    pub source: String,
    /// `branch_protection` or `ruleset`.
    pub source_type: &'static str,
    #[serde(skip)]
    pub source_index: usize,
    /// A review requirement (bypassable via `bypass_pull_request_allowances`).
    #[serde(skip)]
    pub review: bool,
}

/// Result of evaluating merge requirements.
#[derive(Debug, Clone, Default)]
pub struct Evaluation {
    /// Unmet requirements; non-empty ⇒ `blocked`.
    pub blockers: Vec<Blocker>,
    /// Strict status checks and the head is not up to date with the base.
    pub behind: bool,
    /// Non-required checks failing or pending.
    pub unstable: bool,
    pub approvals: i64,
    pub changes_requested: bool,
}

impl Evaluation {
    /// Distinct blocker messages, in order.
    pub fn messages(&self) -> Vec<String> {
        dedup_messages(self.blockers.iter())
    }

    /// Blockers `actor` can't bypass.
    pub fn unbypassed<'a>(&'a self, rules: &'a Rules, actor: &'a Actor) -> Vec<&'a Blocker> {
        self.blockers
            .iter()
            .filter(|b| !rules.bypassed_by(b, actor))
            .collect()
    }
}

/// The rule suite of a merge attempt (`bgh_repos::rule_eval`): one
/// evaluation per `pull_request` / `required_status_checks` rule of each
/// ruleset among `rules`' sources; `None` without rulesets.
pub fn merge_suite(
    repo_id: i64,
    rules: &Rules,
    ev: &Evaluation,
    actor: &Actor,
    base_ref: &str,
    before: &str,
    after: &str,
) -> Option<bgh_repos::rule_eval::SuiteRecord> {
    use bgh_repos::rule_eval::{Eval, SuiteRecord, Violation};
    let mut evals = Vec::new();
    for (i, src) in rules.sources.iter().enumerate() {
        let Source::Ruleset(r) = &src.source else {
            continue;
        };
        for ty in ["pull_request", "required_status_checks"] {
            if r.find_rule(ty).is_none() {
                continue;
            }
            let items: Vec<String> = ev
                .blockers
                .iter()
                .filter(|b| b.source_index == i)
                .filter(|b| {
                    let review = b.review || b.message.contains("conversation");
                    review == (ty == "pull_request")
                })
                .map(|b| b.message.clone())
                .collect();
            evals.push(Eval {
                ruleset_id: r.id,
                ruleset_name: r.name.clone(),
                enforcement: r.enforcement.clone(),
                bypassed: r.bypass_mode(actor) != "never",
                rule_type: ty.to_string(),
                failure: (!items.is_empty()).then(|| Violation {
                    message: items.join(" "),
                    items: vec![],
                }),
            });
        }
    }
    (!evals.is_empty()).then(|| SuiteRecord {
        repo_id,
        actor_id: Some(actor.user_id),
        refname: format!("refs/heads/{base_ref}"),
        before_sha: before.to_string(),
        after_sha: after.to_string(),
        evals,
    })
}

fn dedup_messages<'a>(blockers: impl Iterator<Item = &'a Blocker>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in blockers {
        if !out.contains(&b.message) {
            out.push(b.message.clone());
        }
    }
    out
}

/// The 405 message for blockers that stop a merge: GitHub reports ruleset
/// violations as `Repository rule violations found` followed by one
/// paragraph per rule, classic protection with the first failing rule.
pub fn violation_message(blockers: &[&Blocker]) -> String {
    if blockers.iter().any(|b| b.source_type == "ruleset") {
        let mut msg = "Repository rule violations found\n\n".to_string();
        for m in dedup_messages(blockers.iter().copied()) {
            msg.push_str(&m);
            msg.push_str("\n\n");
        }
        msg
    } else {
        blockers
            .first()
            .map(|b| b.message.clone())
            .unwrap_or_default()
    }
}

/// One reviewer's latest decisive review.
#[derive(Debug, Clone)]
pub struct ReviewState {
    pub user_id: i64,
    pub state: String,
    pub commit_id: Option<String>,
}

/// Latest decisive review state per reviewer (APPROVED / CHANGES_REQUESTED;
/// a later DISMISSED review clears it).
pub async fn review_states(db: &sqlx::PgPool, pull_id: i64) -> ApiResult<Vec<ReviewState>> {
    let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT ON (user_id) user_id, state, commit_id FROM pr_reviews
          WHERE pull_id = $1 AND user_id IS NOT NULL
            AND state IN ('APPROVED', 'CHANGES_REQUESTED', 'DISMISSED')
          ORDER BY user_id, submitted_at DESC NULLS LAST, id DESC",
    )
    .bind(pull_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(_, s, _)| s != "DISMISSED")
        .map(|(user_id, state, commit_id)| ReviewState {
            user_id,
            state,
            commit_id,
        })
        .collect())
}

/// Facts shared by all sources, computed once per evaluation.
struct Facts {
    approvers: HashSet<i64>,
    changes_requested: bool,
    /// Someone other than the last pusher approved the current head.
    last_push_approved: bool,
    missing_owners: Vec<String>,
    outcomes: CheckOutcomes,
    behind: bool,
    unresolved: i64,
    /// Latest deployment state of the head commit per environment
    /// (lowercased name), when a source requires deployments.
    deployments: HashMap<String, Option<String>>,
}

pub async fn evaluate(
    state: &AppState,
    repo: &db::Repository,
    pull: &Pull,
    rules: &Rules,
) -> ApiResult<Evaluation> {
    let db = &state.db;
    let any = |f: &dyn Fn(&SourceRules) -> bool| rules.sources.iter().any(f);

    // Reviews.
    let reviews = review_states(db, pull.id()).await?;
    let mut approvers: HashSet<i64> = HashSet::new();
    let mut head_approvers: Vec<i64> = Vec::new();
    let mut changes_requested = false;
    for r in &reviews {
        if Some(r.user_id) == pull.issue.author_id {
            continue;
        }
        let perm = crate::pulls::member_permission(state, repo, r.user_id).await?;
        if perm < Permission::Write {
            continue;
        }
        match r.state.as_str() {
            "APPROVED" => {
                approvers.insert(r.user_id);
                if r.commit_id.as_deref() == Some(pull.pr.head_sha.as_str()) {
                    head_approvers.push(r.user_id);
                }
            }
            "CHANGES_REQUESTED" => changes_requested = true,
            _ => {}
        }
    }
    let last_push_approved = if any(&|s| {
        s.reviews
            .as_ref()
            .is_some_and(|r| r.require_last_push_approval)
    }) {
        let last_pusher: Option<i64> = sqlx::query_scalar(
            "SELECT coalesce(p.last_pusher_id, i.author_id)
               FROM pull_requests p JOIN issues i ON i.id = p.issue_id WHERE p.issue_id = $1",
        )
        .bind(pull.id())
        .fetch_one(db)
        .await?;
        head_approvers.iter().any(|u| Some(*u) != last_pusher)
    } else {
        true
    };
    let missing_owners = if any(&|s| {
        s.reviews
            .as_ref()
            .is_some_and(|r| r.require_code_owner_reviews)
    }) {
        codeowners::missing_owner_approvals(state, repo, pull, &approvers).await?
    } else {
        vec![]
    };

    // Status checks (base repository only).
    let outcomes = check_outcomes(db, pull.pr.repo_id, &pull.pr.head_sha).await?;
    let behind = if any(&|s| s.checks.as_ref().is_some_and(|c| c.strict)) {
        let store = git::store(state);
        !bgh_git::merge::is_ancestor(
            &store,
            pull.pr.repo_id,
            &pull.pr.base_sha,
            &pull.pr.head_sha,
        )
        .await?
    } else {
        false
    };

    // Conversations.
    let unresolved: i64 = if any(&|s| s.conversation_resolution) {
        sqlx::query_scalar(
            "SELECT count(*) FROM pr_review_comments c
              LEFT JOIN pr_reviews r ON r.id = c.review_id
              WHERE c.pull_id = $1 AND c.in_reply_to_id IS NULL AND c.resolved_at IS NULL
                AND (r.id IS NULL OR r.state <> 'PENDING')",
        )
        .bind(pull.id())
        .fetch_one(db)
        .await?
    } else {
        0
    };

    // Deployments (required_deployments).
    let deployments: HashMap<String, Option<String>> =
        if any(&|s| !s.required_deployments.is_empty()) {
            bgh_core::deployments::latest_for_sha(db, pull.pr.repo_id, &pull.pr.head_sha)
                .await?
                .into_iter()
                .map(|d| (d.environment.to_lowercase(), d.state))
                .collect()
        } else {
            HashMap::new()
        };

    let facts = Facts {
        deployments,
        approvers,
        changes_requested,
        last_push_approved,
        missing_owners,
        outcomes,
        behind,
        unresolved,
    };
    let mut ev = Evaluation {
        behind: facts.behind,
        approvals: facts.approvers.len() as i64,
        changes_requested: facts.changes_requested,
        ..Evaluation::default()
    };
    for (i, s) in rules.sources.iter().enumerate() {
        evaluate_source(i, s, &facts, &mut ev.blockers);
    }
    let required = rules
        .checks
        .as_ref()
        .map(CheckRules::contexts)
        .unwrap_or_default();
    ev.unstable = facts
        .outcomes
        .by_name()
        .iter()
        .any(|(k, o)| *o != CheckOutcome::Success && !required.iter().any(|r| r == k));
    Ok(ev)
}

fn evaluate_source(index: usize, s: &SourceRules, f: &Facts, out: &mut Vec<Blocker>) {
    let ruleset = matches!(s.source, Source::Ruleset(_));
    let mut push = |message: String, review: bool| {
        out.push(Blocker {
            message,
            source: s.source.label(),
            source_type: s.source.kind(),
            source_index: index,
            review,
        })
    };
    if let Some(r) = &s.reviews {
        if f.changes_requested {
            push(
                "Changes requested by a reviewer with write access.".into(),
                true,
            );
        }
        let n = r.required_approving_review_count;
        if (f.approvers.len() as i64) < n {
            push(
                format!(
                    "At least {n} approving review{} is required by reviewers with write access.",
                    if n == 1 { "" } else { "s" }
                ),
                true,
            );
        }
        if r.require_code_owner_reviews && !f.missing_owners.is_empty() {
            push(
                format!(
                    "Waiting on code owner review from {}.",
                    f.missing_owners.join(", ")
                ),
                true,
            );
        }
        if r.require_last_push_approval && !f.last_push_approved {
            push(
                "Approval from someone other than the last pusher is required.".into(),
                true,
            );
        }
    }
    if let Some(c) = &s.checks {
        for k in &c.checks {
            let ctx = &k.context;
            match f.outcomes.get(ctx, k.app_id) {
                None => push(
                    format!("Required status check \"{ctx}\" is expected."),
                    false,
                ),
                Some(CheckOutcome::Pending) => push(
                    format!("Required status check \"{ctx}\" is in progress."),
                    false,
                ),
                Some(CheckOutcome::Failure) => push(
                    format!("Required status check \"{ctx}\" is failing."),
                    false,
                ),
                Some(CheckOutcome::Success) => {}
            }
        }
        if c.strict && f.behind {
            push(
                "Head branch is out of date with the base branch.".into(),
                false,
            );
        }
    }
    if s.conversation_resolution && f.unresolved > 0 {
        push(
            if ruleset {
                "A conversation must be resolved before this pull request can be merged."
            } else {
                "All comments must be resolved."
            }
            .into(),
            false,
        );
    }
    for env in &s.required_deployments {
        match f.deployments.get(&env.to_lowercase()) {
            // `inactive`: succeeded, then superseded by a later deployment.
            Some(Some(state)) if matches!(state.as_str(), "success" | "inactive") => {}
            Some(Some(state)) if matches!(state.as_str(), "failure" | "error") => push(
                format!("Required deployment to \"{env}\" has failed."),
                false,
            ),
            Some(_) => push(
                format!("Required deployment to \"{env}\" is in progress."),
                false,
            ),
            None => push(
                format!("Required deployment to \"{env}\" is expected."),
                false,
            ),
        }
    }
    // P25 (required_signatures) and P39 (merge_queue) add their checks here.
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
