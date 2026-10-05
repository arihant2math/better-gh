//! Ruleset evaluation of pushes: every rule of every ruleset selecting a
//! ref update (active and `evaluate` ones, bypassed or not) yields an
//! [`Eval`]; failures of active rulesets the actor can't bypass reject the
//! push with GitHub's `GH013` report, and each update is recorded as a
//! rule suite (`rule_suites`, served by [`crate::rule_suites`]).
//!
//! Evaluation runs in two phases:
//!
//! * [`ref_evals`] before git sees the pack: rules decided by the ref
//!   alone (`creation`, `update`, `deletion`, `pull_request`,
//!   `required_status_checks`, `branch_name_pattern`, `tag_name_pattern`);
//! * [`object_evals`] from the `pre-receive` hook while the pushed objects
//!   are quarantined: `non_fast_forward`, `required_linear_history`, the
//!   metadata rules (`commit_message_pattern`,
//!   `commit_author_email_pattern`, `committer_email_pattern`) and the
//!   push rules (`file_path_restriction`, `max_file_size`,
//!   `file_extension_restriction`, `max_file_path_length`).
//!
//! `merge_queue`, `required_deployments`, `workflows`, `code_scanning` and
//! `required_signatures` are stored but not evaluated here.

use bgh_core::events::RefUpdate;
use bgh_core::prelude::*;
use bgh_git::{ChangedFile, GitCli, PushedCommit};
use serde_json::{Value, json};

use crate::protection::{Actor, RepoRules, RulesetRow, missing_status_checks, pattern_matches};

/// Rule types evaluated against the pushed objects ([`object_evals`]).
pub const OBJECT_RULES: &[&str] = &[
    "non_fast_forward",
    "required_linear_history",
    "commit_message_pattern",
    "commit_author_email_pattern",
    "committer_email_pattern",
    "file_path_restriction",
    "max_file_size",
    "file_extension_restriction",
    "max_file_path_length",
];

/// Outcome of one rule of one ruleset for one ref update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eval {
    pub ruleset_id: i64,
    pub ruleset_name: String,
    /// `active` or `evaluate`.
    pub enforcement: String,
    /// The actor may bypass the ruleset (for direct pushes).
    pub bypassed: bool,
    pub rule_type: String,
    /// `None`: the rule passed.
    pub failure: Option<Violation>,
}

/// Why a rule failed: GitHub's message plus the offending commits / paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub message: String,
    pub items: Vec<String>,
}

impl Violation {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            items: vec![],
        }
    }
}

impl Eval {
    fn new(r: &RulesetRow, bypassed: bool, rule_type: &str, failure: Option<Violation>) -> Self {
        Self {
            ruleset_id: r.id,
            ruleset_name: r.name.clone(),
            enforcement: r.enforcement.clone(),
            bypassed,
            rule_type: rule_type.to_string(),
            failure,
        }
    }

    /// Fails and is enforced against this actor.
    pub fn blocks(&self) -> bool {
        self.failure.is_some() && self.enforcement == "active" && !self.bypassed
    }

    /// `rule_evaluations[]` entry of a rule suite.
    pub fn to_json(&self) -> Value {
        json!({
            "rule_source": {"type": "ruleset", "id": self.ruleset_id, "name": self.ruleset_name},
            "enforcement": self.enforcement,
            "result": if self.failure.is_some() { "fail" } else { "pass" },
            "rule_type": self.rule_type,
            "details": self.failure.as_ref().map(|f| f.message.clone()),
        })
    }
}

/// Every ruleset (active, then `evaluate`) selecting `u.refname`.
fn rulesets_for<'a>(
    rules: &'a RepoRules,
    refname: &'a str,
) -> impl Iterator<Item = &'a RulesetRow> {
    rules
        .push_rulesets_for(refname)
        .chain(rules.evaluate_for(refname))
}

/// Whether any ruleset selecting one of `updates` has a rule that needs
/// the pushed objects.
pub fn needs_objects(rules: &RepoRules, updates: &[RefUpdate]) -> bool {
    updates.iter().filter(|u| !u.is_delete()).any(|u| {
        rulesets_for(rules, &u.refname)
            .any(|r| OBJECT_RULES.iter().any(|t| r.find_rule(t).is_some()))
    })
}

/// Whether any ruleset selects any of `updates` (worth a rule suite).
pub fn any_ruleset(rules: &RepoRules, updates: &[RefUpdate]) -> bool {
    updates
        .iter()
        .any(|u| rulesets_for(rules, &u.refname).next().is_some())
}

fn rules_of(r: &RulesetRow) -> impl Iterator<Item = &Value> {
    r.rules.as_array().into_iter().flatten()
}

fn ref_kind(refname: &str) -> &'static str {
    if refname.starts_with("refs/tags/") {
        "tag"
    } else {
        "branch"
    }
}

// ----- metadata patterns -------------------------------------------------------------

/// Whether `value` satisfies a metadata rule's `{operator, pattern,
/// negate}` parameters. Invalid regexes never match.
pub fn pattern_ok(params: &Value, value: &str) -> bool {
    let pattern = params["pattern"].as_str().unwrap_or_default();
    let matched = match params["operator"].as_str().unwrap_or_default() {
        "starts_with" => value.starts_with(pattern),
        "ends_with" => value.ends_with(pattern),
        "contains" => value.contains(pattern),
        "regex" => regex::Regex::new(pattern).is_ok_and(|re| re.is_match(value)),
        _ => true,
    };
    matched != params["negate"].as_bool().unwrap_or(false)
}

/// GitHub's description of a metadata rule (its `name` when set).
pub fn pattern_message(rule_type: &str, params: &Value) -> String {
    if let Some(name) = params["name"].as_str().filter(|n| !n.trim().is_empty()) {
        return name.to_string();
    }
    let subject = match rule_type {
        "commit_message_pattern" => "Commit message",
        "commit_author_email_pattern" => "Commit author email address",
        "committer_email_pattern" => "Committer email address",
        "branch_name_pattern" => "Branch name",
        _ => "Tag name",
    };
    let verb = match params["operator"].as_str().unwrap_or_default() {
        "starts_with" => "start with a matching pattern",
        "ends_with" => "end with a matching pattern",
        "contains" => "contain a matching pattern",
        _ => "match a given regex pattern",
    };
    let not = if params["negate"].as_bool().unwrap_or(false) {
        "not "
    } else {
        ""
    };
    format!(
        "{subject} must {not}{verb}: {}",
        params["pattern"].as_str().unwrap_or_default()
    )
}

// ----- phase 1: ref rules ------------------------------------------------------------

/// Evaluate the rules decided by the ref update alone.
pub async fn ref_evals(
    state: &AppState,
    rules: &RepoRules,
    actor: &Actor,
    u: &RefUpdate,
) -> ApiResult<Vec<Eval>> {
    let mut out = Vec::new();
    let what = ref_kind(&u.refname);
    let short = u
        .refname
        .strip_prefix("refs/heads/")
        .or_else(|| u.refname.strip_prefix("refs/tags/"))
        .unwrap_or(&u.refname);
    for r in rulesets_for(rules, &u.refname) {
        let bypassed = r.bypassed_by(actor);
        for rule in rules_of(r) {
            let ty = rule["type"].as_str().unwrap_or_default();
            let p = &rule["parameters"];
            let failure = match ty {
                "creation" if u.is_create() => Some(Violation::new(
                    "Cannot create ref due to creations being restricted.",
                )),
                "update" if !u.is_create() && !u.is_delete() => {
                    Some(Violation::new("Cannot update this protected ref."))
                }
                "deletion" if u.is_delete() => {
                    Some(Violation::new(format!("Cannot delete this {what}")))
                }
                "creation" | "update" | "deletion" => continue,
                "pull_request" if !u.is_delete() => Some(Violation::new(
                    "Changes must be made through a pull request.",
                )),
                "required_status_checks" if !u.is_delete() => {
                    if u.is_create() && p["do_not_enforce_on_create"].as_bool() == Some(true) {
                        continue;
                    }
                    let contexts: Vec<String> = p["required_status_checks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c["context"].as_str().map(str::to_string))
                        .collect();
                    let missing =
                        missing_status_checks(state, rules.repo_id, &u.new, &contexts).await?;
                    match missing.len() {
                        0 => None,
                        1 => Some(Violation::new(format!(
                            "Required status check {:?} is expected.",
                            missing[0]
                        ))),
                        n => Some(Violation {
                            message: format!(
                                "{n} of {} required status checks are expected.",
                                contexts.len()
                            ),
                            items: missing,
                        }),
                    }
                }
                "branch_name_pattern" | "tag_name_pattern" if u.is_create() => {
                    let wanted = if ty == "branch_name_pattern" {
                        "branch"
                    } else {
                        "tag"
                    };
                    if what != wanted {
                        continue;
                    }
                    (!pattern_ok(p, short)).then(|| Violation::new(pattern_message(ty, p)))
                }
                _ => continue,
            };
            out.push(Eval::new(r, bypassed, ty, failure));
        }
    }
    Ok(out)
}

// ----- phase 2: object rules ---------------------------------------------------------

/// Lazily read new commits and changed files of one update.
struct Pushed<'a> {
    git: &'a GitCli,
    envs: &'a [(&'a str, &'a str)],
    new: &'a str,
    commits: Option<Vec<PushedCommit>>,
    files: Option<Vec<ChangedFile>>,
}

impl Pushed<'_> {
    async fn commits(&mut self) -> &[PushedCommit] {
        if self.commits.is_none() {
            let c = match self.git.new_commits(self.new, self.envs).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "rulesets: listing pushed commits failed");
                    vec![]
                }
            };
            self.commits = Some(c);
        }
        self.commits.as_deref().unwrap_or_default()
    }

    async fn files(&mut self) -> &[ChangedFile] {
        if self.files.is_none() {
            let shas: Vec<String> = self.commits().await.iter().map(|c| c.sha.clone()).collect();
            let f = match self.git.changed_files(&shas, self.envs).await {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(error = %e, "rulesets: listing pushed files failed");
                    vec![]
                }
            };
            self.files = Some(f);
        }
        self.files.as_deref().unwrap_or_default()
    }
}

fn violations(message: String, mut items: Vec<String>) -> Option<Violation> {
    if items.is_empty() {
        return None;
    }
    items.dedup();
    Some(Violation { message, items })
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect()
}

/// Whether `path` matches a `file_path_restriction` pattern (fnmatch on
/// the full path, `**` across directories; a pattern without `/` also
/// matches the file name).
fn path_restricted(pattern: &str, path: &str) -> bool {
    pattern_matches(pattern, path)
        || (!pattern.contains('/')
            && pattern_matches(pattern, path.rsplit('/').next().unwrap_or(path)))
}

/// Whether `path` has a restricted extension (`*.exe`, `.exe` or `exe`).
fn extension_restricted(ext: &str, path: &str) -> bool {
    let ext = ext
        .trim_start_matches('*')
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if ext.is_empty() {
        return false;
    }
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.ends_with(&format!(".{ext}"))
}

/// Evaluate the rules that need the pushed objects (`envs`: the
/// quarantine, see `smart_http::QuarantineEnv`).
pub async fn object_evals(
    git: &GitCli,
    envs: &[(&str, &str)],
    rules: &RepoRules,
    actor: &Actor,
    u: &RefUpdate,
) -> Vec<Eval> {
    let mut out = Vec::new();
    if u.is_delete() {
        return out;
    }
    let mut pushed = Pushed {
        git,
        envs,
        new: &u.new,
        commits: None,
        files: None,
    };
    let what = ref_kind(&u.refname);
    for r in rulesets_for(rules, &u.refname) {
        let bypassed = r.bypassed_by(actor);
        for rule in rules_of(r) {
            let ty = rule["type"].as_str().unwrap_or_default();
            if !OBJECT_RULES.contains(&ty) {
                continue;
            }
            let p = &rule["parameters"];
            let failure = match ty {
                "non_fast_forward" => {
                    if u.is_create() {
                        continue;
                    }
                    let ff = git
                        .is_ancestor_with(&u.old, &u.new, envs)
                        .await
                        .unwrap_or(false);
                    (!ff).then(|| Violation::new(format!("Cannot force-push to this {what}")))
                }
                "required_linear_history" => {
                    let merges = pushed
                        .commits()
                        .await
                        .iter()
                        .filter(|c| c.parents > 1)
                        .map(|c| c.sha.clone())
                        .collect();
                    violations(
                        format!("This {what} must not contain merge commits."),
                        merges,
                    )
                }
                "commit_message_pattern"
                | "commit_author_email_pattern"
                | "committer_email_pattern" => {
                    let bad = pushed
                        .commits()
                        .await
                        .iter()
                        .filter(|c| {
                            let v = match ty {
                                "commit_message_pattern" => &c.message,
                                "commit_author_email_pattern" => &c.author_email,
                                _ => &c.committer_email,
                            };
                            !pattern_ok(p, v)
                        })
                        .map(|c| c.sha.clone())
                        .collect();
                    violations(pattern_message(ty, p), bad)
                }
                "file_path_restriction" => {
                    let pats = strings(&p["restricted_file_paths"]);
                    let bad = pushed
                        .files()
                        .await
                        .iter()
                        .filter(|f| pats.iter().any(|pat| path_restricted(pat, &f.path)))
                        .map(|f| f.path.clone())
                        .collect();
                    violations("Cannot update restricted file paths.".into(), bad)
                }
                "file_extension_restriction" => {
                    let exts = strings(&p["restricted_file_extensions"]);
                    let bad = pushed
                        .files()
                        .await
                        .iter()
                        .filter(|f| exts.iter().any(|e| extension_restricted(e, &f.path)))
                        .map(|f| f.path.clone())
                        .collect();
                    violations("Cannot push files with restricted extensions.".into(), bad)
                }
                "max_file_size" => {
                    let mb = p["max_file_size"].as_u64().unwrap_or(100);
                    let bad = pushed
                        .files()
                        .await
                        .iter()
                        .filter(|f| f.size > mb * 1024 * 1024)
                        .map(|f| f.path.clone())
                        .collect();
                    violations(format!("File size must be less than {mb} MB."), bad)
                }
                "max_file_path_length" => {
                    let max = p["max_file_path_length"].as_u64().unwrap_or(256) as usize;
                    let bad = pushed
                        .files()
                        .await
                        .iter()
                        .filter(|f| f.path.chars().count() > max)
                        .map(|f| f.path.clone())
                        .collect();
                    violations(
                        format!("File path length must not exceed {max} characters."),
                        bad,
                    )
                }
                _ => continue,
            };
            out.push(Eval::new(r, bypassed, ty, failure));
        }
    }
    out
}

// ----- reporting ---------------------------------------------------------------------

/// GitHub's `GH013` report of the blocking evaluations of one ref, as
/// `remote:` lines.
pub fn gh013_lines(rules_url: &str, refname: &str, evals: &[Eval]) -> Vec<String> {
    let blocking: Vec<&Eval> = evals.iter().filter(|e| e.blocks()).collect();
    if blocking.is_empty() {
        return vec![];
    }
    let mut lines = vec![
        format!("error: GH013: Repository rule violations found for {refname}."),
        format!(
            "Review all repository rules at {rules_url}?ref={}",
            url_encode(refname)
        ),
        String::new(),
    ];
    let mut seen: Vec<&str> = Vec::new();
    for e in blocking {
        let f = e.failure.as_ref().expect("blocking evals fail");
        if seen.contains(&f.message.as_str()) {
            continue;
        }
        seen.push(&f.message);
        lines.push(format!("- {}", f.message));
        if !f.items.is_empty() {
            let n = f.items.len();
            lines.push(format!(
                "  Found {n} violation{}:",
                if n == 1 { "" } else { "s" }
            ));
            lines.push(String::new());
            for item in f.items.iter().take(20) {
                lines.push(format!("  {item}"));
            }
            if n > 20 {
                lines.push(format!("  ... and {} more", n - 20));
            }
        }
        lines.push(String::new());
    }
    lines
}

fn url_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// ----- rule suites -------------------------------------------------------------------

/// `pass`, `fail` or `bypass` over `evals` (only `active` ones when
/// `active_only`).
pub fn outcome(evals: &[Eval], active_only: bool) -> &'static str {
    let considered = evals
        .iter()
        .filter(|e| !active_only || e.enforcement == "active");
    let mut bypass = false;
    for e in considered {
        if e.failure.is_some() {
            if !e.bypassed {
                return "fail";
            }
            bypass = true;
        }
    }
    if bypass { "bypass" } else { "pass" }
}

/// One evaluated ref update (push) or merge.
#[derive(Debug, Clone)]
pub struct SuiteRecord {
    pub repo_id: i64,
    pub actor_id: Option<i64>,
    pub refname: String,
    pub before_sha: String,
    pub after_sha: String,
    pub evals: Vec<Eval>,
}

/// Store a rule suite (`actor_name` is the actor's login).
pub async fn record(db: &sqlx::PgPool, s: &SuiteRecord) -> ApiResult<i64> {
    let evals: Vec<Value> = s.evals.iter().map(Eval::to_json).collect();
    let actor_id = s.actor_id.filter(|id| *id > 0);
    Ok(sqlx::query_scalar(
        "INSERT INTO rule_suites (repo_id, actor_id, actor_name, ref, before_sha, after_sha,
                                  result, evaluation_result, rule_evaluations)
         VALUES ($1, $2, (SELECT login FROM users WHERE id = $2), $3, $4, $5, $6, $7, $8)
         RETURNING id",
    )
    .bind(s.repo_id)
    .bind(actor_id)
    .bind(&s.refname)
    .bind(&s.before_sha)
    .bind(&s.after_sha)
    .bind(outcome(&s.evals, true))
    .bind(outcome(&s.evals, false))
    .bind(Value::Array(evals))
    .fetch_one(db)
    .await?)
}

/// Record suites, logging (not failing) on database errors: a push or
/// merge must not fail because its audit trail could not be written.
pub async fn record_all(db: &sqlx::PgPool, suites: &[SuiteRecord]) {
    for s in suites {
        if let Err(e) = record(db, s).await {
            tracing::warn!(error = %e, "rulesets: recording a rule suite failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_patterns() {
        let p = json!({"operator": "starts_with", "pattern": "feat"});
        assert!(pattern_ok(&p, "feat: x"));
        assert!(!pattern_ok(&p, "fix: x"));
        let p = json!({"operator": "regex", "pattern": "^[a-z]+@example\\.com$", "negate": false});
        assert!(pattern_ok(&p, "a@example.com"));
        assert!(!pattern_ok(&p, "a@evil.com"));
        let p = json!({"operator": "contains", "pattern": "WIP", "negate": true});
        assert!(!pattern_ok(&p, "WIP: x"));
        assert!(pattern_ok(&p, "done"));
        assert_eq!(
            pattern_message(
                "commit_message_pattern",
                &json!({"operator": "regex", "pattern": "^feat"})
            ),
            "Commit message must match a given regex pattern: ^feat"
        );
        assert_eq!(
            pattern_message(
                "tag_name_pattern",
                &json!({"name": "Semver tags", "operator": "regex", "pattern": "x"})
            ),
            "Semver tags"
        );
    }

    #[test]
    fn push_rule_paths() {
        assert!(path_restricted(
            ".github/workflows/**",
            ".github/workflows/ci.yml"
        ));
        assert!(path_restricted("*.pem", "keys/server.pem"));
        assert!(!path_restricted("secrets/*", "src/secrets/x"));
        assert!(extension_restricted("*.exe", "bin/tool.EXE"));
        assert!(extension_restricted(".jar", "lib/a.jar"));
        assert!(!extension_restricted("exe", "exe"));
    }

    #[test]
    fn outcomes() {
        let e = |enf: &str, bypassed, fail: bool| Eval {
            ruleset_id: 1,
            ruleset_name: "r".into(),
            enforcement: enf.into(),
            bypassed,
            rule_type: "deletion".into(),
            failure: fail.then(|| Violation::new("x")),
        };
        assert_eq!(outcome(&[e("active", false, false)], true), "pass");
        assert_eq!(outcome(&[e("evaluate", false, true)], true), "pass");
        assert_eq!(outcome(&[e("evaluate", false, true)], false), "fail");
        assert_eq!(outcome(&[e("active", true, true)], true), "bypass");
        assert_eq!(
            outcome(&[e("active", true, true), e("active", false, true)], true),
            "fail"
        );
    }
}
