# P3 — PR merge governance: status

Branch `bgh/p03-merge-governance`. **Status: complete** (PHASE4_PLAN §2 P3).

## What changed

* **One evaluator** (`crates/bgh-pulls/src/protection.rs`): `rules_for`
  loads `bgh_repos::protection::RepoRules` (classic + active rulesets) and
  `effective()` builds per-source `SourceRules` (the classic rule
  protecting the branch via `protection_for`, plus every active ruleset
  selecting `refs/heads/<base>`). Merged values (`Rules::reviews`,
  `checks`, `conversation_resolution`, `linear_history`) are the most
  restrictive of all sources (approval count max, flags or-ed, checks
  unioned; ruleset `dismiss_stale_reviews_on_push` feeds the synchronize
  dismissal, ruleset `required_linear_history` forbids merge commits).
  Used by `PUT /merge`, `mergeable_state` (mergeability refresh),
  auto-merge and the `/_bgh` requirements endpoint.
* **Blockers carry their source** (`Blocker { message, source,
  source_type }`) and are bypassed per source: classic by admins unless
  `enforce_admins`, review requirements also by
  `bypass_pull_request_allowances`; rulesets by bypass actors in either
  `always` or `pull_request` mode.
* **Merge API**: 405 `Repository rule violations found\n\n<rule>\n\n...`
  when any unbypassed blocker comes from a ruleset (GitHub's text, shown
  by `gh pr merge`); classic-only violations keep the first message
  (e.g. `At least 1 approving review is required by reviewers with write
  access.`). Classic push `restrictions` gate merge: 405 `You're not
  authorized to push to this branch.` (admins bypass unless
  `enforce_admins`).
* **Fork-status spoofing fixed**: `check_outcomes(db, base_repo_id, sha)`
  only reads statuses / check runs of the base repository.
* **Expected source**: classic `checks[].app_id` and ruleset
  `integration_id` only accept check runs from that integration
  (`checks::app_id_for_slug`: Actions = 1, REST API = 2; P17 brings real
  app ids). `-1` / absent = any source (statuses included).
* **`require_last_push_approval`** (classic and ruleset): an approval of
  the current head by someone other than the last pusher is required.
  Message: `Approval from someone other than the last pusher is
  required.`
* **Thread resolution**: ruleset `required_review_thread_resolution`
  (message `A conversation must be resolved before this pull request can
  be merged.`) next to classic `required_conversation_resolution`.
* **`dismissal_restrictions`**: `PUT .../reviews/{id}/dismissals` returns
  403 `You are not allowed to dismiss reviews on this branch.` unless the
  caller is admin or listed (users / teams).
* **Merge box**: `GET /_bgh/repos/{o}/{r}/pulls/{n}/requirements` adds
  `requirements: [{message, source, source_type}]`; `blockers` stays (now
  deduplicated messages); `can_bypass` = the viewer could merge despite
  every current blocker. `MergeBox.tsx` shows `message (source)`
  (text-only change); mock server updated.

## Tables / migrations

* `migrations/1500_pr_last_pusher.sql`: `pull_requests.last_pusher_id`
  (set by synchronize when the head changes; NULL = PR author).

## Shared-code changes (additive)

* `bgh-repos/src/protection.rs`: `RulesetRow::find_rule`,
  `RulesetRow::bypass_mode` (moved from `rulesets.rs`, which now
  delegates), `Actor::for_user`, `Actor::matches_bypass_actor`,
  `Actor::is_listed_in`.
* `bgh-pulls/src/checks.rs`: `pub fn app_id_for_slug`.

## Tests

`crates/bgh-pulls/tests/it/governance.rs`: ruleset (2 approvals + `ci`)
blocks merge and auto-merge until both pass; ruleset bypass actor merges;
fork-spoof (base `ci=failure`, fork `ci=success` status + check run) stays
blocked; `app_id` mismatch doesn't satisfy; last-push approval;
dismissal restrictions (403) and push restrictions; bypass PR allowances
skip reviews but not checks.

## Hooks / known gaps

* `required_signatures` is captured on `SourceRules` but not enforced
  (P25); `merge_queue` (P39) and `required_deployments` (P20) plug into
  `evaluate_source`. Org rulesets (P23) plug into `RepoRules::load`.
* Statuses have no app identity, so a required check with an expected
  app only matches check runs.
* Restrictions on `apps` are not evaluated (no GitHub Apps until P17).
* Dismiss review UI is P74's.
