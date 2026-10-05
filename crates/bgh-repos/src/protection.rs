//! Branch protection and ruleset evaluation for ref updates.
//!
//! [`RepoRules`] holds a repository's classic protection rules
//! (`branch_protections`) and rulesets (`repo_rulesets`). [`check_update`]
//! decides whether an [`Actor`] may apply one ref update; what can only be
//! verified with the objects or statuses is returned as [`Needs`]:
//!
//! * git pushes turn `Needs` into a [`PushPolicy`] enforced by a
//!   `pre-receive` hook on the quarantined objects (force pushes, linear
//!   history) and check required statuses in the database before accepting
//!   the pack;
//! * API writes (refs, contents, merges) verify `Needs` directly
//!   ([`verify_needs`]).
//!
//! Semantics follow GitHub: admins bypass classic rules unless
//! `enforce_admins`, except "allow force pushes" and "allow deletions",
//! which apply to everyone. Rulesets apply to everyone not listed in their
//! `bypass_actors`; `evaluate` rulesets are not enforced.

use bgh_core::events::RefUpdate;
use bgh_core::perms::{Permission, RepoAccess};
use bgh_core::prelude::*;
use bgh_git::smart_http::{HookVerdict, ObjectCheck, PushPolicy, QuarantineEnv};

use crate::rule_eval;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// `branch_protections` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProtectionRow {
    pub id: i64,
    pub repo_id: i64,
    pub pattern: String,
    pub required_status_checks: Option<Value>,
    pub required_pull_request_reviews: Option<Value>,
    pub restrictions: Option<Value>,
    pub enforce_admins: bool,
    pub required_linear_history: bool,
    pub allow_force_pushes: bool,
    pub allow_deletions: bool,
    pub block_creations: bool,
    pub required_conversation_resolution: bool,
    pub required_signatures: bool,
    pub lock_branch: bool,
    pub allow_fork_syncing: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// "Require deployments to succeed before merging" (P20).
    pub required_deployment_environments: Vec<String>,
}

impl ProtectionRow {
    pub const COLUMNS: &'static str = "id, repo_id, pattern, required_status_checks, \
        required_pull_request_reviews, restrictions, enforce_admins, required_linear_history, \
        allow_force_pushes, allow_deletions, block_creations, required_conversation_resolution, \
        required_signatures, lock_branch, allow_fork_syncing, created_at, updated_at, \
        required_deployment_environments";

    /// Required status check contexts (`contexts` plus `checks[].context`).
    pub fn required_contexts(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(v) = &self.required_status_checks {
            for c in v["contexts"].as_array().into_iter().flatten() {
                if let Some(s) = c.as_str() {
                    out.push(s.to_string());
                }
            }
            for c in v["checks"].as_array().into_iter().flatten() {
                if let Some(s) = c["context"].as_str() {
                    out.push(s.to_string());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// `repo_rulesets` row: a repository ruleset, or an organization ruleset
/// (`org_id` set, `repo_id` 0) applying to the repositories its
/// `repository_name` / `repository_id` conditions select.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RulesetRow {
    pub id: i64,
    pub repo_id: i64,
    pub org_id: Option<i64>,
    pub name: String,
    pub target: String,
    pub enforcement: String,
    pub conditions: Value,
    pub rules: Value,
    pub bypass_actors: Value,
    pub created_by_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl RulesetRow {
    pub const COLUMNS: &'static str = "id, coalesce(repo_id, 0) AS repo_id, org_id, name, \
        target, enforcement, conditions, rules, bypass_actors, created_by_id, created_at, \
        updated_at";

    /// `Organization` or `Repository`.
    pub fn source_type(&self) -> &'static str {
        if self.org_id.is_some() {
            "Organization"
        } else {
            "Repository"
        }
    }

    /// Whether an organization ruleset's conditions select `repo`
    /// (`repository_name` include/exclude fnmatch patterns or `~ALL`,
    /// or `repository_id.repository_ids`). Repository rulesets select
    /// only their own repository.
    pub fn applies_to_repo(&self, repo: &db::Repository) -> bool {
        if self.org_id.is_none() {
            return self.repo_id == repo.id;
        }
        let c = &self.conditions;
        if let Some(ids) = c["repository_id"]["repository_ids"].as_array() {
            return ids.iter().any(|v| v.as_i64() == Some(repo.id));
        }
        let names = &c["repository_name"];
        if !names.is_object() {
            return false;
        }
        let name = repo.name.to_ascii_lowercase();
        let matches = |p: &str| p == "~ALL" || pattern_matches(&p.to_ascii_lowercase(), &name);
        let list = |k: &str| -> Vec<String> {
            names[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        };
        list("include").iter().any(|p| matches(p)) && !list("exclude").iter().any(|p| matches(p))
    }

    /// Whether the ruleset's conditions select `refname`. Push rulesets
    /// apply to every ref.
    pub fn applies_to(&self, refname: &str, default_branch: &str) -> bool {
        if self.target == "push" {
            return true;
        }
        let prefix = if self.target == "tag" {
            "refs/tags/"
        } else {
            "refs/heads/"
        };
        if !refname.starts_with(prefix) {
            return false;
        }
        let cond = &self.conditions["ref_name"];
        let matches = |p: &str| match p {
            "~ALL" => true,
            "~DEFAULT_BRANCH" => refname == format!("refs/heads/{default_branch}"),
            p if p.starts_with("refs/") => pattern_matches(p, refname),
            p => pattern_matches(&format!("{prefix}{p}"), refname),
        };
        let list = |k: &str| -> Vec<String> {
            cond[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        };
        list("include").iter().any(|p| matches(p)) && !list("exclude").iter().any(|p| matches(p))
    }

    fn rule(&self, ty: &str) -> Option<&Value> {
        self.rules
            .as_array()?
            .iter()
            .find(|r| r["type"].as_str() == Some(ty))
    }

    /// The rule of type `ty` (`{"type", "parameters"}`), if present.
    pub fn find_rule(&self, ty: &str) -> Option<&Value> {
        self.rule(ty)
    }

    /// `"always"`, `"pull_requests_only"` or `"never"`: how `actor` may
    /// bypass this ruleset. Either bypassing mode allows merging a pull
    /// request without meeting the ruleset's requirements.
    pub fn bypass_mode(&self, actor: &Actor) -> &'static str {
        let mut best = "never";
        for b in self.bypass_actors.as_array().into_iter().flatten() {
            if !actor.matches_bypass_actor(b) {
                continue;
            }
            if b["bypass_mode"].as_str() == Some("pull_request") {
                best = "pull_requests_only";
            } else {
                return "always";
            }
        }
        best
    }

    /// Whether `actor` may bypass this ruleset for direct pushes
    /// (`bypass_mode: pull_request` only covers pull request merges).
    pub fn bypassed_by(&self, actor: &Actor) -> bool {
        self.bypass_actors
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["bypass_mode"].as_str() != Some("pull_request"))
            .any(|b| actor.matches_bypass_actor(b))
    }
}

/// fnmatch-style branch pattern: `*` (no `/`), `**` (anything), `?`.
pub fn pattern_matches(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = &p[2..];
                (0..=n.len()).any(|i| go(rest, &n[i..]))
            }
            Some(b'*') => {
                let rest = &p[1..];
                for i in 0..=n.len() {
                    if go(rest, &n[i..]) {
                        return true;
                    }
                    if i < n.len() && n[i] == b'/' {
                        break;
                    }
                }
                false
            }
            Some(b'?') => !n.is_empty() && n[0] != b'/' && go(&p[1..], &n[1..]),
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}

/// All protection rules and rulesets of a repository.
#[derive(Debug, Clone, Default)]
pub struct RepoRules {
    pub repo_id: i64,
    pub protections: Vec<ProtectionRow>,
    /// Active rulesets of the repository and of its organization (those
    /// selecting the repository).
    pub rulesets: Vec<RulesetRow>,
    /// Rulesets in `evaluate` mode: evaluated and recorded in rule
    /// suites, never enforced.
    pub evaluate: Vec<RulesetRow>,
    pub default_branch: String,
}

impl RepoRules {
    /// Two indexed queries.
    pub async fn load(db: &sqlx::PgPool, repo: &db::Repository) -> ApiResult<Self> {
        let protections = sqlx::query_as(&format!(
            "SELECT {} FROM branch_protections WHERE repo_id = $1 ORDER BY id",
            ProtectionRow::COLUMNS
        ))
        .bind(repo.id)
        .fetch_all(db)
        .await?;
        let all: Vec<RulesetRow> = sqlx::query_as(&format!(
            "SELECT {} FROM repo_rulesets
              WHERE (repo_id = $1 OR org_id = $2) AND enforcement IN ('active', 'evaluate')
              ORDER BY id",
            RulesetRow::COLUMNS
        ))
        .bind(repo.id)
        .bind(repo.owner_id)
        .fetch_all(db)
        .await?;
        let (rulesets, evaluate) = all
            .into_iter()
            .filter(|r| r.applies_to_repo(repo))
            .partition(|r| r.enforcement == "active");
        Ok(Self {
            repo_id: repo.id,
            protections,
            rulesets,
            evaluate,
            default_branch: repo.default_branch.clone(),
        })
    }

    /// No classic rule and no active ruleset (`evaluate` rulesets aside).
    pub fn is_empty(&self) -> bool {
        self.protections.is_empty() && self.rulesets.is_empty()
    }

    /// Nothing at all to evaluate on a push (classic, active or evaluate).
    pub fn is_unruled(&self) -> bool {
        self.is_empty() && self.evaluate.is_empty()
    }

    /// `evaluate` rulesets selecting `refname`.
    pub fn evaluate_for<'a>(&'a self, refname: &'a str) -> impl Iterator<Item = &'a RulesetRow> {
        self.evaluate
            .iter()
            .filter(move |r| r.applies_to(refname, &self.default_branch))
    }

    /// The classic rule protecting `branch`: an exact name beats patterns,
    /// longer patterns beat shorter ones.
    pub fn protection_for(&self, branch: &str) -> Option<&ProtectionRow> {
        self.protections
            .iter()
            .find(|p| p.pattern == branch)
            .or_else(|| {
                self.protections
                    .iter()
                    .filter(|p| pattern_matches(&p.pattern, branch))
                    .max_by_key(|p| (p.pattern.len(), std::cmp::Reverse(p.id)))
            })
    }

    /// Active branch / tag rulesets selecting `refname` (push rulesets,
    /// which only restrict pushed content, are left out: see
    /// [`Self::push_rulesets_for`]).
    pub fn rulesets_for<'a>(&'a self, refname: &'a str) -> impl Iterator<Item = &'a RulesetRow> {
        self.push_rulesets_for(refname)
            .filter(|r| r.target != "push")
    }

    /// Every active ruleset evaluated on a push to `refname`, push
    /// rulesets included.
    pub fn push_rulesets_for<'a>(
        &'a self,
        refname: &'a str,
    ) -> impl Iterator<Item = &'a RulesetRow> {
        self.rulesets
            .iter()
            .filter(move |r| r.applies_to(refname, &self.default_branch))
    }
}

/// Who is updating refs.
#[derive(Debug, Clone)]
pub struct Actor {
    pub user_id: i64,
    pub permission: Permission,
    /// Teams the user belongs to, including parents of those teams.
    pub team_ids: Vec<i64>,
    pub org_admin: bool,
    /// Pushing with a deploy key (`DeployKey` bypass actors).
    pub deploy_key: bool,
    /// Acting as a GitHub App / integration (`Integration` bypass actors).
    pub integration_id: Option<i64>,
}

impl Actor {
    pub async fn load(state: &AppState, access: &RepoAccess, user: &db::User) -> ApiResult<Self> {
        Self::for_user(state, &access.owner, user.id, access.permission).await
    }

    /// [`Actor::load`] from the repository owner, a user id and the user's
    /// effective permission on the repository.
    pub async fn for_user(
        state: &AppState,
        owner: &db::User,
        user_id: i64,
        permission: Permission,
    ) -> ApiResult<Self> {
        let team_ids: Vec<i64> = sqlx::query_scalar(
            "WITH RECURSIVE t AS (
                SELECT tm.team_id AS id FROM team_members tm
                  JOIN teams x ON x.id = tm.team_id
                 WHERE tm.user_id = $1 AND x.org_id = $2
                UNION
                SELECT p.parent_id FROM teams p JOIN t ON p.id = t.id WHERE p.parent_id IS NOT NULL
             ) SELECT id FROM t",
        )
        .bind(user_id)
        .bind(owner.id)
        .fetch_all(&state.db)
        .await?;
        let org_admin = owner.is_org()
            && bgh_core::perms::org_role(&state.db, owner.id, user_id)
                .await?
                .as_deref()
                == Some("admin");
        Ok(Self {
            user_id,
            permission,
            team_ids,
            org_admin,
            deploy_key: false,
            integration_id: None,
        })
    }

    /// A deploy key pushing with `access` (no user: never listed in
    /// restrictions or bypass lists).
    pub fn deploy_key(access: &RepoAccess) -> Self {
        Self {
            user_id: 0,
            permission: access.permission,
            team_ids: Vec::new(),
            org_admin: false,
            deploy_key: true,
            integration_id: None,
        }
    }

    /// Whether one `bypass_actors` entry selects this actor (any mode).
    pub fn matches_bypass_actor(&self, b: &Value) -> bool {
        let id = b["actor_id"].as_i64();
        match b["actor_type"].as_str() {
            Some("RepositoryRole") => {
                let role = match id {
                    Some(1) => Permission::Read,
                    Some(2) => Permission::Maintain,
                    Some(3) => Permission::Triage,
                    Some(4) => Permission::Write,
                    Some(5) => Permission::Admin,
                    _ => return false,
                };
                self.permission >= role
            }
            Some("OrganizationAdmin") => self.org_admin,
            Some("Team") => id.is_some_and(|t| self.team_ids.contains(&t)),
            Some("User") => self.user_id != 0 && id == Some(self.user_id),
            Some("DeployKey") => self.deploy_key,
            Some("Integration") => id.is_some() && id == self.integration_id,
            _ => false,
        }
    }

    /// Whether the actor is listed in a `{"users": [ids], "teams": [ids]}`
    /// value (push restrictions, dismissal restrictions, bypass
    /// allowances). `None` / `null` lists nobody.
    pub fn is_listed_in(&self, v: Option<&Value>) -> bool {
        self.listed_in(v)
    }

    fn listed_in(&self, v: Option<&Value>) -> bool {
        let Some(v) = v else { return false };
        let has = |k: &str, id: i64| {
            v[k].as_array()
                .into_iter()
                .flatten()
                .any(|x| x.as_i64() == Some(id))
        };
        has("users", self.user_id) || self.team_ids.iter().any(|t| has("teams", *t))
    }
}

/// The GitHub App acting through `auth` (app JWT or installation token),
/// matched by `Integration` bypass actors.
pub async fn integration_of(state: &AppState, auth: &AuthContext) -> ApiResult<Option<i64>> {
    if let Some(app) = bgh_core::apps::jwt_app_id(auth) {
        return Ok(Some(app));
    }
    let Some(installation) = bgh_core::apps::installation_id(auth) else {
        return Ok(None);
    };
    Ok(
        sqlx::query_scalar("SELECT app_id FROM app_installations WHERE id = $1")
            .bind(installation)
            .fetch_optional(&state.db)
            .await?,
    )
}

/// Checks an allowed update still needs (objects / statuses).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Needs {
    /// The update must be a fast-forward.
    pub fast_forward: bool,
    /// New commits must not contain merges.
    pub linear: bool,
    /// Status check contexts that must have succeeded on the new commit.
    pub status_contexts: Vec<String>,
}

impl Needs {
    fn merge(&mut self, o: Needs) {
        self.fast_forward |= o.fast_forward;
        self.linear |= o.linear;
        self.status_contexts.extend(o.status_contexts);
        self.status_contexts.sort();
        self.status_contexts.dedup();
    }
}

fn declined(msg: &str) -> String {
    format!("protected branch hook declined: {msg}")
}

fn check_classic(p: &ProtectionRow, actor: &Actor, u: &RefUpdate) -> Result<Needs, String> {
    let mut needs = Needs::default();
    if u.is_delete() {
        return if p.allow_deletions {
            Ok(needs)
        } else {
            Err(declined("Cannot delete this protected branch."))
        };
    }
    let bypass = actor.permission >= Permission::Admin && !p.enforce_admins;
    if !p.allow_force_pushes {
        needs.fast_forward = true;
    }
    if bypass {
        return Ok(needs);
    }
    if p.lock_branch {
        return Err(declined(
            "Cannot update this protected branch: it is locked.",
        ));
    }
    if u.is_create() && p.block_creations {
        return Err(declined("Cannot create this protected branch."));
    }
    if p.restrictions.is_some() && !actor.listed_in(p.restrictions.as_ref()) {
        return Err(declined("You're not authorized to push to this branch."));
    }
    if let Some(pr) = &p.required_pull_request_reviews
        && !actor.listed_in(Some(&pr["bypass_pull_request_allowances"]))
    {
        return Err(declined("Changes must be made through a pull request."));
    }
    needs.linear = p.required_linear_history;
    needs.status_contexts = p.required_contexts();
    Ok(needs)
}

fn check_ruleset(r: &RulesetRow, actor: &Actor, u: &RefUpdate) -> Result<Needs, String> {
    let mut needs = Needs::default();
    if r.bypassed_by(actor) {
        return Ok(needs);
    }
    let what = if r.target == "tag" { "tag" } else { "branch" };
    let fail = |m: String| {
        Err(format!(
            "push declined due to repository rule violations: {m}"
        ))
    };
    if u.is_delete() {
        if r.rule("deletion").is_some() {
            return fail(format!("Cannot delete this {what} (ruleset {:?}).", r.name));
        }
        return Ok(needs);
    }
    if u.is_create() && r.rule("creation").is_some() {
        return fail(format!("Cannot create this {what} (ruleset {:?}).", r.name));
    }
    if !u.is_create() && r.rule("update").is_some() {
        return fail(format!("Cannot update this {what} (ruleset {:?}).", r.name));
    }
    if r.rule("pull_request").is_some() {
        return fail("Changes must be made through a pull request.".into());
    }
    needs.fast_forward = r.rule("non_fast_forward").is_some();
    needs.linear = r.rule("required_linear_history").is_some();
    if let Some(rule) = r.rule("required_status_checks") {
        for c in rule["parameters"]["required_status_checks"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(ctx) = c["context"].as_str() {
                needs.status_contexts.push(ctx.to_string());
            }
        }
    }
    Ok(needs)
}

/// Decide whether `actor` may apply `u`. `Err(reason)` rejects.
pub fn check_update(rules: &RepoRules, actor: &Actor, u: &RefUpdate) -> Result<Needs, String> {
    let mut needs = Needs::default();
    if let Some(branch) = u.branch()
        && let Some(p) = rules.protection_for(branch)
    {
        needs.merge(check_classic(p, actor, u)?);
    }
    for r in rules.rulesets_for(&u.refname) {
        needs.merge(check_ruleset(r, actor, u)?);
    }
    if u.is_create() || u.is_delete() {
        needs.fast_forward = false;
    }
    Ok(needs)
}

/// Context names (from `contexts`) that have not succeeded on `sha`: the
/// latest commit status must be `success`, or a completed check run with
/// that name must have concluded `success`, `neutral` or `skipped`.
pub async fn missing_status_checks(
    state: &AppState,
    repo_id: i64,
    sha: &str,
    contexts: &[String],
) -> ApiResult<Vec<String>> {
    if contexts.is_empty() {
        return Ok(vec![]);
    }
    Ok(sqlx::query_scalar(
        "SELECT ctx FROM unnest($3::text[]) AS ctx
          WHERE coalesce((SELECT s.state FROM commit_statuses s
                           WHERE s.repo_id = $1 AND s.sha = $2 AND s.context = ctx
                           ORDER BY s.id DESC LIMIT 1), '') <> 'success'
            AND NOT EXISTS (SELECT 1 FROM check_runs c
                             WHERE c.repo_id = $1 AND c.head_sha = $2 AND c.name = ctx
                               AND c.status = 'completed'
                               AND c.conclusion IN ('success', 'neutral', 'skipped'))
          ORDER BY ctx",
    )
    .bind(repo_id)
    .bind(sha)
    .bind(contexts)
    .fetch_all(&state.db)
    .await?)
}

fn status_reason(missing: &[String]) -> String {
    let list = missing
        .iter()
        .map(|c| format!("{c:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    declined(&format!("Required status check {list} is expected."))
}

/// Authorize a git push: classic rule checks per update and required
/// status checks in the database, the hook policy for object checks, and
/// ruleset evaluation ([`crate::rule_eval`]): ref rules now, object rules
/// from the `pre-receive` hook ([`PushPolicy::object_check`]). Ruleset
/// violations are reported in GitHub's `GH013` format, and every update
/// selected by a ruleset is recorded as a rule suite.
pub async fn authorize_push(
    state: &AppState,
    rules: &RepoRules,
    actor: &Actor,
    updates: &[RefUpdate],
) -> Result<PushPolicy, String> {
    let internal = |_| "internal error checking repository rules".to_string();
    let mut policy = PushPolicy::default();
    for u in updates {
        let mut needs = Needs::default();
        if let Some(branch) = u.branch()
            && let Some(p) = rules.protection_for(branch)
        {
            needs = check_classic(p, actor, u)?;
        }
        if u.is_create() || u.is_delete() {
            needs.fast_forward = false;
        }
        if !needs.status_contexts.is_empty() {
            let missing =
                missing_status_checks(state, rules.repo_id, &u.new, &needs.status_contexts)
                    .await
                    .map_err(internal)?;
            if !missing.is_empty() {
                return Err(status_reason(&missing));
            }
        }
        if needs.fast_forward {
            policy.no_force_push.push(u.refname.clone());
        }
        if needs.linear {
            policy.linear_history.push(u.refname.clone());
        }
    }
    if !rule_eval::any_ruleset(rules, updates) {
        return Ok(policy);
    }

    let mut ref_evals = Vec::with_capacity(updates.len());
    for u in updates {
        ref_evals.push(
            rule_eval::ref_evals(state, rules, actor, u)
                .await
                .map_err(internal)?,
        );
    }
    let suites = |evals: Vec<Vec<rule_eval::Eval>>| -> Vec<rule_eval::SuiteRecord> {
        updates
            .iter()
            .zip(evals)
            .filter(|(u, _)| rule_eval::any_ruleset(rules, std::slice::from_ref(*u)))
            .map(|(u, evals)| rule_eval::SuiteRecord {
                repo_id: rules.repo_id,
                actor_id: Some(actor.user_id),
                refname: u.refname.clone(),
                before_sha: u.old.clone(),
                after_sha: u.new.clone(),
                evals,
            })
            .collect()
    };
    if ref_evals.iter().flatten().any(rule_eval::Eval::blocks) {
        let url = rules_url(state, rules.repo_id).await;
        let mut lines = Vec::new();
        for (u, evals) in updates.iter().zip(&ref_evals) {
            lines.extend(rule_eval::gh013_lines(&url, &u.refname, evals));
        }
        rule_eval::record_all(&state.db, &suites(ref_evals)).await;
        return Err(format!(
            "push declined due to repository rule violations\n{}",
            lines.join("\n")
        ));
    }
    if !rule_eval::needs_objects(rules, updates) {
        rule_eval::record_all(&state.db, &suites(ref_evals)).await;
        return Ok(policy);
    }

    let records = std::sync::Arc::new(suites(ref_evals));
    let (state, rules, actor) = (state.clone(), rules.clone(), actor.clone());
    policy.object_check = Some(ObjectCheck::new(move |env: QuarantineEnv| {
        let (state, rules, actor, records) =
            (state.clone(), rules.clone(), actor.clone(), records.clone());
        async move {
            let git = match crate::store(&state).cli(rules.repo_id) {
                Ok(g) => g,
                Err(e) => {
                    tracing::error!(error = %e, "rulesets: opening the repository failed");
                    return HookVerdict {
                        accept: false,
                        lines: vec!["error: internal error checking repository rules".into()],
                    };
                }
            };
            let envs = env.envs();
            let mut records = (*records).clone();
            for s in &mut records {
                let u = RefUpdate {
                    old: s.before_sha.clone(),
                    new: s.after_sha.clone(),
                    refname: s.refname.clone(),
                };
                s.evals
                    .extend(rule_eval::object_evals(&git, &envs, &rules, &actor, &u).await);
            }
            let mut lines = Vec::new();
            if records
                .iter()
                .flat_map(|s| &s.evals)
                .any(rule_eval::Eval::blocks)
            {
                let url = rules_url(&state, rules.repo_id).await;
                for s in &records {
                    lines.extend(rule_eval::gh013_lines(&url, &s.refname, &s.evals));
                }
            }
            rule_eval::record_all(&state.db, &records).await;
            HookVerdict {
                accept: lines.is_empty(),
                lines,
            }
        }
    }));
    Ok(policy)
}

/// `{html}/{owner}/{repo}/rules` (the "Review all repository rules" link).
async fn rules_url(state: &AppState, repo_id: i64) -> String {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT u.login, r.name FROM repositories r JOIN users u ON u.id = r.owner_id
          WHERE r.id = $1",
    )
    .bind(repo_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    match row {
        Some((owner, name)) => format!("{}/rules", state.urls.repo_html(&owner, &name)),
        None => state.urls.html("/"),
    }
}

/// Verify [`Needs`] for an API-side ref update using the repository's
/// objects. Errors are 422s with GitHub's messages.
pub async fn verify_needs(
    state: &AppState,
    git: &bgh_git::GitCli,
    repo_id: i64,
    u: &RefUpdate,
    needs: &Needs,
) -> ApiResult<()> {
    if u.is_delete() {
        return Ok(());
    }
    if needs.fast_forward && !u.is_create() && !git.is_ancestor(&u.old, &u.new).await? {
        return Err(ApiError::unprocessable("Cannot force-push to this branch"));
    }
    if needs.linear {
        let mut args = vec![
            "rev-list",
            "--min-parents=2",
            "--max-count=1",
            u.new.as_str(),
        ];
        let exclude = format!("^{}", u.old);
        if u.is_create() {
            args.extend(["--not", "--all"]);
        } else {
            args.push(&exclude);
        }
        let merges = git.run(&args, &[], None).await?;
        if !String::from_utf8_lossy(&merges).trim().is_empty() {
            return Err(ApiError::unprocessable(
                "This branch must not contain merge commits.",
            ));
        }
    }
    let missing = missing_status_checks(state, repo_id, &u.new, &needs.status_contexts).await?;
    if !missing.is_empty() {
        return Err(ApiError::unprocessable(format!(
            "Required status check {} is expected.",
            missing
                .iter()
                .map(|c| format!("{c:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(())
}

// ----- compatibility with the pre-engine API ----------------------------------------

/// Load a repository's rules by id (compat for callers of the original
/// `load_rules`; prefer [`RepoRules::load`]).
pub async fn load_rules(state: &AppState, repo_id: i64) -> ApiResult<RepoRules> {
    let repo = db::Repository::find(&state.db, repo_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    RepoRules::load(&state.db, &repo).await
}

/// Synchronous rule check without team membership or object checks
/// (compat). New code should use [`Actor::load`] + [`authorize_push`] and
/// enforce the returned policy with `smart_http::receive_pack_with_policy`.
pub fn check_push(
    rules: &RepoRules,
    access: &RepoAccess,
    pusher: &AuthContext,
    updates: &[RefUpdate],
) -> Result<(), String> {
    check_push_by(rules, access, Some(pusher.user.id), updates)
}

/// [`check_push`] for a pusher identified by user id; `None` for deploy
/// keys, which never match push restrictions or bypass lists.
pub fn check_push_by(
    rules: &RepoRules,
    access: &RepoAccess,
    pusher_id: Option<i64>,
    updates: &[RefUpdate],
) -> Result<(), String> {
    let actor = Actor {
        user_id: pusher_id.unwrap_or(0),
        permission: access.permission,
        team_ids: vec![],
        org_admin: false,
        deploy_key: pusher_id.is_none(),
        integration_id: None,
    };
    for u in updates {
        check_update(rules, &actor, u)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::pattern_matches as m;

    #[test]
    fn patterns() {
        assert!(m("main", "main"));
        assert!(!m("main", "mainline"));
        assert!(m("release/*", "release/1.0"));
        assert!(!m("release/*", "release/1.0/hotfix"));
        assert!(m("release/**", "release/1.0/hotfix"));
        assert!(m("v?", "v1"));
        assert!(m("*", "feature"));
        assert!(!m("*", "feature/x"));
    }
}
