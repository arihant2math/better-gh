# P23 — Rulesets, part 2 backend: status

Branch `bgh/p23-rulesets`. **Status: complete** (PHASE4_PLAN §3 P23),
self-integrated into `claude/sleepy-cray-9jj0t3`.

## Endpoints

| Endpoint | Notes |
|---|---|
| `GET/POST /orgs/{org}/rulesets` | Org owners (`admin:org`); members 403, others 404. `targets=` filter. |
| `GET/PUT/DELETE /orgs/{org}/rulesets/{id}` | PUT is a partial update (Terraform sends full bodies). |
| `GET /repos/{o}/{r}/rulesets` | Now `includes_parents` (default true: org rulesets selecting the repo) and `targets=`. |
| `GET /repos/{o}/{r}/rulesets/{id}` | Also serves org rulesets selecting the repo (read-only through the repo; writes 404). |
| `GET /repos/{o}/{r}/rules/branches/{b}` | Includes org rules (`ruleset_source_type: Organization`, `ruleset_source: <org>`); push rulesets excluded. |
| `GET /repos/{o}/{r}/rulesets/rule-suites[/{id}]` | Repo admins. Filters `ref`, `time_period`, `actor_name`, `rule_suite_result`; Link pagination. |
| `GET /orgs/{org}/rulesets/rule-suites[/{id}]` | Org owners; plus `repository_name`. |
| `GET/POST /repos/{o}/{r}/tags/protection`, `DELETE .../{id}` | Legacy tag protection mapped onto tag rulesets (see below). |
| GraphQL `Repository.rulesets`, `Organization.rulesets` | `RepositoryRuleset` (databaseId, name, target, enforcement, source union, rules connection with `type`), enough for `gh ruleset list [--org]`. Org field needs an owner with `admin:org` (gh's scope hint message). |

`gh ruleset list/view/check` (repo and `--org`) are covered in
`scripts/gh-compat.sh` (new `-- rulesets` section + fixtures).

## Rules and bypass actors

* All GitHub rule types are accepted and normalized (round trip of a
  Terraform `github_organization_ruleset` payload is tested):
  `creation`, `update` (`update_allows_fetch_and_merge`), `deletion`,
  `required_linear_history`, `required_signatures`, `non_fast_forward`,
  `pull_request` (now also `allowed_merge_methods`),
  `required_status_checks` (`do_not_enforce_on_create`), metadata rules
  (`commit_message_pattern`, `commit_author_email_pattern`,
  `committer_email_pattern`, `branch_name_pattern`, `tag_name_pattern`:
  operator starts_with/ends_with/contains/regex, `negate`, `name`; regexes
  validated), push rules (`file_path_restriction`, `max_file_size` 1–100,
  `file_extension_restriction`, `max_file_path_length` 1–256), and
  stored-only `merge_queue` (P39), `required_deployments` (P20),
  `workflows` (P30), `code_scanning` (P66).
* `target: push` rulesets (push rules only; no `ref_name`; apply to every
  ref). Push rules are also allowed in branch/tag rulesets.
* Org conditions: `repository_name` include/exclude (fnmatch,
  case-insensitive, `~ALL`) + `protected`, or `repository_id`
  `repository_ids`; `repository_property` is stored but never matches
  (custom properties are deferred).
* Bypass actors: `DeployKey` (actor_id null), `Integration` (app id;
  matches once P17 gives actors an integration id), plus the existing
  `RepositoryRole`, `OrganizationAdmin`, `Team`, `User`; modes `always`,
  `pull_request`, `exempt` (treated like always).

## Enforcement (`bgh_repos::rule_eval`)

* `RepoRules::load` now loads the repo's and its org's rulesets (one
  query; org ones filtered by `applies_to_repo`) and splits active ones
  (`rulesets`) from `evaluate` ones (`evaluate`). So merges (P3's
  evaluator), `check_update` (API ref writes) and branch listings see org
  rulesets too. `rulesets_for` excludes push rulesets;
  `push_rulesets_for` includes them.
* `protection::authorize_push`: classic rules as before; rulesets are
  evaluated per update into `Eval`s (every rule of every active/evaluate
  ruleset, bypass recorded). Ref rules run before git sees the pack; any
  active, unbypassed failure rejects with GitHub's report:
  `error: GH013: Repository rule violations found for <ref>.` /
  `Review all repository rules at <html>/rules?ref=…` / `- <rule message>`
  / `Found N violations:` + shas or paths. Object rules run from the
  pre-receive hook via a server callback (below). `evaluate` rulesets
  never block.
* Pre-receive callback (`bgh_git::smart_http`): `PushPolicy.object_check`
  (`ObjectCheck`, `QuarantineEnv`, `HookVerdict`). The server creates two
  FIFOs (`BGH_CHECK_DIR`); the hook writes its `GIT_OBJECT_DIRECTORY` /
  `GIT_ALTERNATE_OBJECT_DIRECTORIES`, the server evaluates in Rust
  (`GitCli::new_commits`, `GitCli::changed_files`,
  `GitCli::is_ancestor_with`, new `bgh_git::pushed`) and answers exit
  code + `remote:` lines. Fails closed (no answer = reject). Works for
  HTTP and SSH. `ensure_hooks` now verifies the hook is executable
  (`access(X_OK)`, repairs the mode, else refuses the push): git silently
  skips a non-executable hook, which would disable every object check.
* Multi-line rejection reasons: `smart_http::rejection_report` sends the
  first line as the per-ref `ng` reason and the rest verbatim on the
  progress channel.

## Rule suites

* Table `rule_suites` (one row per evaluated ref update or merge attempt):
  `result` (active rules: pass/fail/bypass), `evaluation_result` (active +
  evaluate), `rule_evaluations` (GitHub's `rule_source`/`enforcement`/
  `result`/`rule_type`/`details`). Recorded for every update selected by
  at least one ruleset, including rejected ones; recording errors are
  logged, never fail the push.
* Merges: `bgh_pulls::protection::merge_suite` (additive) records each
  `perform_merge` evaluation (REST merge and auto-merge) with
  `pull_request` / `required_status_checks` evaluations per ruleset.

## Legacy tag protection

Each pattern is an active tag ruleset `Tag protection: <pattern>`
(`tag_protection = true`) with `creation`, `update`, `deletion` and a
`RepositoryRole` 2 (maintain) bypass, so maintainers and admins can still
manage matching tags. The tag protection id is the ruleset id.

## Migrations

`migrations/3500_rulesets_org_suites.sql`: `repo_rulesets.repo_id`
nullable + `org_id` (exactly one set; org and repo rulesets share the id
sequence), `tag_protection`, target `push`, `org_rulesets_name_key`;
table `rule_suites` (+ index `(repo_id, pushed_at DESC, id DESC)`).

## Shared-code changes (additive)

* Workspace dep `libc` (bgh-git, `mkfifo`); bgh-repos uses `regex`.
* `bgh-git`: `smart_http::{ObjectCheck, QuarantineEnv, HookVerdict,
  PushPolicy::object_check}`, hook section for `BGH_CHECK_DIR`,
  `pushed::{PushedCommit, ChangedFile}`, `GitCli::{new_commits,
  changed_files, is_ancestor_with}`.
* `bgh-repos/src/protection.rs`: `RulesetRow::{org_id, source_type,
  applies_to_repo}` (COLUMNS now `coalesce(repo_id, 0) AS repo_id, org_id,
  …`), `RepoRules::{evaluate, evaluate_for, push_rulesets_for,
  is_unruled}`, `Actor::{deploy_key, integration_id}` (new fields — code
  constructing `Actor` literally must set them).
* `bgh-pulls`: `protection::merge_suite`, one call in `merge.rs`.
* `bgh-graphql`: new `model/ruleset.rs`; one `rulesets` field each on
  `Repository` and `Organization`.
* `docs/ARCHITECTURE.md`: ruleset evaluation in the smart-HTTP paragraph.

## Tests

* `bgh-repos` `tests/it/org_rulesets.rs`: org CRUD + Terraform round trip
  + permissions + validation; org rulesets applied to selected repos
  (rules/branches, includes_parents, read-through, repository_id);
  push rulesets and new bypass actor types; tag protection API.
* `tests/it/push_rules.rs`: push rules (GH013 for size, extension, path,
  path length; admins too; rule suites recorded with evaluations);
  metadata rules + evaluate mode + rule-suite API (filters, pagination,
  detail, permissions); branch name and tag protection pushes; org
  rulesets enforced on push + org rule suites.
* `tests/it/ssh.rs::deploy_key_bypasses_rulesets` (needs `ssh`).
* `tests/it/protection.rs`: updated for `allowed_merge_methods` and new
  validations.
* `bgh-pulls` governance: merge attempts recorded as rule suites.
* `bgh-graphql` `tests/it/rulesets.rs`: gh's exact list queries.
* Unit tests: metadata patterns, push rule path matching, outcomes.

## Known gaps

* API-side writes (contents API, web edits, refs API) still use the
  pre-P23 ref checks: org/repo ruleset ref rules apply, but metadata and
  push rules are not evaluated and no rule suites are recorded.
* Merges only record `pull_request` / `required_status_checks`
  evaluations and don't evaluate `evaluate`-mode rulesets.
* `required_signatures` is accepted but not enforced (P25).
* Webhooks `repository_ruleset` / org audit-log streaming not emitted
  (P10/P64 territory); rulesets still emit `RepositoryUpdated` and audit
  entries `repository_ruleset.*`.
* Rule suites are not pruned (no retention job yet).
* GraphQL `RepositoryRule.parameters`, `conditions`, `bypassActors` are
  not exposed (P44).
* Web mock backend: no rulesets endpoints (P24 adds them with the UI).
