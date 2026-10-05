# P10 webhook-wiring — status

**Done.** Self-integrated (fast-forward) into `claude/sleepy-cray-9jj0t3` with
the full gate green (fmt, clippy, `cargo test --workspace`, web
typecheck/lint/test/build, `api-smoke.sh`, `gh-compat.sh`).
Branch `bgh/p10-webhook-wiring`. Scope: `docs/PHASE4_PLAN.md` §P10 (no §5
quick fixes are assigned to P10). Builds on P9's outbox: every new emit is
a `tx.emit` (durable) except wiki writes, which are git commits and use
`state.events.emit` (outbox background writer).

## Webhooks now produced

| Webhook (action) | Source |
|---|---|
| `star` created/deleted, `watch` started | `RepositoryStarred` (bgh-repos stars) now mapped |
| `repository` edited (real `changes`: description, homepage, default_branch, topics), archived, unarchived, publicized, privatized, transferred (`changes.owner.from.user\|organization`); `public` | new `RepositoryEdited`; `RepositoryArchived/Unarchived/Publicized/Privatized` emitted by `settings.rs` (`PATCH`, topics) and admin `manage_repos.rs`; `RepositoryTransferred` mapped (also emitted by the admin transfer). Fixes the activity `PublicEvent`. `RepositoryUpdated` (protection, rulesets, sync, code index) no longer maps to `repository` edited. |
| `member` added/edited (`changes.permission.from/to`)/removed | `CollaboratorEdited` / `CollaboratorRemoved` emitted by `collaborators.rs` |
| `release` edited (`changes.name/body/tag_name/make_latest`), released, prereleased, unpublished | `ReleaseEdited` and new `ReleaseStateChanged` emitted by `releases.rs` PATCH |
| `check_run` created/completed/rerequested/requested_action, `check_suite` requested/rerequested/completed | Checks-API variants (`CheckRunCreated`, …) mapped; new `CheckRunActionRequested` from `POST /_bgh/repos/{o}/{r}/check-runs/{id}/requested-action` |
| `ci_activity` notification for failed Checks-API suites | `CheckSuiteCompleted` in `fanout.rs`; recipient = pusher of the head commit (latest `PushEvent`), else author of an open PR at that head |
| `team` created/edited/deleted/added_to_repository/removed_from_repository, `team_add`, `membership` added/removed, `organization` member_removed/member_invited | `payloads/org.rs`; delivered to org hooks and global hooks (`team_add` and the team repo grants also to the repo's hooks) |
| `issues` pinned/unpinned/transferred | mapped; `transferred` goes to the **old** repository with `changes.new_issue` / `new_repository` |
| `pull_request` auto_merge_enabled/disabled, `pull_request_review` edited (`changes.body.from`), `pull_request_review_thread` resolved/unresolved (`thread.{node_id, comments}`) | mapped |
| `sub_issues` sub_issue_added/removed (parent repo), parent_issue_added/removed (sub-issue repo) | mapped (cross-repo scopes) |
| `gollum` (pages created/edited) | new `WikiPagesUpdated` from `bgh-wiki` create/update/revert/delete. Deletes are emitted as domain events (action `deleted`) but produce no delivery: GitHub's `gollum` has no deleted action |
| `deploy_key` created/deleted | new `DeployKeyCreated/Deleted` (key REST JSON) from `keys.rs` |
| `branch_protection_rule` created/edited (`changes`)/deleted | new `BranchProtectionRuleChanged` (GitHub `rule` object, `protection_api::webhook_rule_json`) |
| `repository_ruleset` created/edited (`changes.name/enforcement/conditions/rules`)/deleted | new `RepositoryRulesetChanged` from `rulesets.rs` |
| `meta` deleted | sent to a hook subscribed to `meta` when it is deleted, via the `notify.deliver_meta` job (the hook row and its delivery log cascade away, so the job carries URL/secret/body; not in the delivery log) |
| Payload fidelity | `issue_comment` / `pull_request_review_comment` edited carry `changes.body.from`; deleted ones carry the full comment (snapshot taken before the delete, carried in the event) |

## Shared-code changes (additive)

* `bgh-core/src/events.rs`: new variants `RepositoryEdited`,
  `ReleaseStateChanged`, `DeployKeyCreated`, `DeployKeyDeleted`,
  `BranchProtectionRuleChanged`, `RepositoryRulesetChanged`,
  `WikiPagesUpdated`, `CheckRunActionRequested`; new `#[serde(default)]`
  fields `IssueCommentEdited.changes`, `IssueCommentDeleted.comment`,
  `PullRequestReviewEdited.changes`, `PullRequestReviewCommentEdited.changes`,
  `PullRequestReviewCommentDeleted.comment`, `TeamDeleted.team` (old outbox
  rows still decode; builders fall back to `{}` / the minimal object).
* `bgh-core/src/sync/shapes.rs`: `checkRun` rows include `actions`.
* `bgh-notify`: `payloads/mod.rs` (owner) new arms, `payloads/org.rs` (new),
  issues/pulls builders, `webhooks/dispatch.rs` (multi-repo/org scopes:
  org events, old owner on transfer, old repo on issue transfer, sub-issue
  repo), `webhooks/deliver.rs` (`DeliverMeta`), `fanout.rs` (`ci_failed`).
* Emit points: bgh-repos (`settings.rs`, `collaborators.rs`, `keys.rs`,
  `rulesets.rs`, `protection_api.rs`), bgh-releases `releases.rs`, bgh-wiki
  `api.rs`, bgh-issues `comments.rs`, bgh-pulls (`comments.rs`, `reviews.rs`,
  `checks.rs`, route in `lib.rs`), bgh-accounts `teams.rs` (delete snapshot),
  bgh-admin `manage_repos.rs`.

## Web

Pull request Checks tab: a run's `actions` render as buttons (write access)
that call the requested-action endpoint (`requestCheckRunAction`); mock
route + a seeded action on failing mock runs.

## Migrations

`2200_webhook_wiring.sql`: partial index
`activity_events (repo_id, payload->>'head') WHERE type = 'PushEvent'` for
the `ci_activity` pusher lookup.

## Tests

* `crates/bgh-notify/tests/it/wiring.rs`: one test per plan bullet — each
  performs the action over the API and asserts the `webhook_deliveries` row
  (event, action, required top-level keys, key values). The org test also
  checks that a global hook subscribed to `team` receives team creation.
* `crates/bgh-notify/tests/it/coverage.rs`: every name in
  `webhooks::EVENTS` is either produced (a sample domain event maps to it
  via `event_names` **and** a source scan finds a crate constructing that
  variant) or listed in `NOT_PRODUCIBLE_YET` / `HOOK_LIFECYCLE`. A produced
  event that is still listed fails too, so the list shrinks as features land.
* `webhooks.rs::meta_deleted_is_sent_to_the_deleted_hook` (real receiver,
  signature header).

## Known gaps

* `gollum` is not emitted for wiki `git push` (only web/API edits).
* `organization` `member_removed` reports `role: "member"` (the membership row
  is gone before dispatch).
* `repository_ruleset` `changes` uses `{"from": old}` for conditions/rules
  rather than GitHub's added/deleted/updated breakdown.
* Org-level rulesets (P23) will need their own emit.
