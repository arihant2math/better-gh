Integration: landed
Hide/minimize comments (issue, PR review, review and commit comments), edit history with revision deletion, and issue deletion (REST `/_bgh`, GraphQL, web UI). Full gate green after merging `claude/sleepy-cray-9jj0t3` (e82e659).

# P42 comment-moderation — status

Branch `bgh/p42-comment-moderation`. Scope: `docs/PHASE4_PLAN.md` §P42
(no §5 quick fixes are assigned to P42). Evidence: `docs/AUDIT.md`
"Moderation gaps".

## Schema (migration `5400_comment_moderation.sql`)

* `minimized_reason` (`spam|abuse|off-topic|outdated|duplicate|resolved`,
  GraphQL `minimizedReason` spelling), `minimized_by_id`, `minimized_at` on
  `comments`, `pr_review_comments`, `pr_reviews`, `commit_comments`.
* `user_content_edits(repo_id, target_type, target_id, editor_id, body,
  previous_body, created_at, deleted_at, deleted_by_id)` — one row per
  edit of an issue/PR body (`issue`), conversation comment (`comment`),
  review body (`review`), review comment (`review_comment`) or commit
  comment (`commit_comment`). `body` = text after the edit, `previous_body`
  = before. AFTER DELETE triggers on the five content tables drop a
  target's history (also on cascades, e.g. an issue's comments).
* `deleted_issues(repo_id, number, deleted_by_id, deleted_at)`: the number
  stays reserved (`next_issue_number` never goes back) and answers 410.

## Shared code (additive)

* `bgh_core::moderation` (new): `ContentKind`, `parse_reason`,
  `find_target`, `set_minimized`, `minimized_reasons`, `record_edit`,
  `edits_for`, `delete_revision`, `delete_original`, `render_edits`.
* `bgh_core::sync::shapes`: `comment`, `review`, `reviewComment` carry
  `minimizedReason`; `issue` carries `bodyEditedAt` with the lazy `body`
  (docs/SYNC_PROTOCOL.md updated).
* `bgh_core::node_id::NodeType::UserContentEdit` (new variant, appended).
* `bgh_core::token_permissions`: `deleteIssue` (issues: write),
  `minimizeComment` / `unminimizeComment` (issues or pull_requests: write).

## Edit recording (same transaction as the edit)

`bgh-issues` issue body (`PATCH /issues/{n}`) and comments; `bgh-pulls` PR
body, review body (submitted reviews only), review comments (not while
their review is pending); `bgh-repos` commit comments (the PATCH now runs
in a `Tx`). GraphQL mutations go through those handlers. Transfers move
the history's `repo_id` along.

## Endpoints (bgh-issues `moderation.rs`, web routes)

| Endpoint | Notes |
|---|---|
| `PUT /_bgh/repos/{o}/{r}/minimized/{kind}/{id}` `{"reason"}` | triage+; 422 bad/missing reason; `kind` ∈ `comment, review, review_comment, commit_comment` (else 404); pending review content 404/422; audited `comment.minimize`; syncs the row |
| `DELETE /_bgh/repos/{o}/{r}/minimized/{kind}/{id}` | unhide, same rules |
| `GET /_bgh/repos/{o}/{r}/minimized/{kind}?ids=` | `[{"id","minimizedReason"}]` (commit comments aren't synced) |
| `GET /_bgh/repos/{o}/{r}/edits/{kind}/{id}` | history newest first (`kind` also `issue`, by issue id) |
| `DELETE /_bgh/repos/{o}/{r}/edits/{kind}/{id}/{edit_id}` | content author or repo admin; current revision → 422; `edit_id` 0 = original text; audited |
| `DELETE /_bgh/repos/{o}/{r}/issues/{n}` | repo admin (org owners are admins); PRs → 422; removes the issue, its comments, reactions, notifications, subscriptions; `D` sync + related issues/PRs/repo/milestone re-synced; `Event::IssueDeleted` (webhook `issues.deleted`); audited `issue.destroy` |

`GET /repos/{o}/{r}/issues/{n}` (and everything using `issues::load`)
returns `410 {"message":"This issue was deleted"}` for a deleted number.
Search and lists read the `issues` table, so the issue disappears there.

## GraphQL (bgh-graphql)

* New `model/moderation.rs`: `Minimizable` interface (IssueComment,
  PullRequestReviewComment, PullRequestReview, CommitComment),
  `UserContentEdit` (+ connection; `diff` = text after the edit), minimal
  `CommitComment`, batch loaders `Loaders::minimized` / `content_edits`.
* `isMinimized`, `minimizedReason`, `viewerCanMinimize`, `lastEditedAt`,
  `editor`, `includesCreatedEdit`, `userContentEdits` are real on Issue,
  PullRequest, IssueComment, PullRequestReview, PullRequestReviewComment.
* New `mutation/moderation.rs`: `minimizeComment`, `unminimizeComment`
  (`ReportedContentClassifiers`), `deleteIssue`.

## Web (all in lazy route chunks; initial JS unchanged)

* `sync/models.ts` / `schema.ts`: `minimizedReason` on `Comment`, `Review`,
  `ReviewComment`; lazy `Issue.bodyEditedAt`; `CLIENT_SCHEMA_VERSION` 3.
* `sync/moderation.ts`: optimistic `setMinimized` (comment, review
  comment, review) and `deleteIssue`; `api/moderation.ts`: edit history,
  revision deletion, commit-comment hidden states.
* `pages/issues/Moderation.tsx` (own lazy chunk): "edited ▾" revision
  dropdown + revision viewer (delete revision for author/admin) and the
  "Hide comment" reason dialog.
* `Timeline.tsx` (comment card / menu region only): Hide / Unhide menu
  items (triage+), collapsed "This comment was marked as … Show comment",
  "edited ▾" on the issue body and comments; hidden reviews collapse too.
* `ReviewThread.tsx` (review comments) and `commits/CommitComments.tsx`
  (commit comments): the same hide / unhide / edited ▾.
* `IssueActions.tsx` sidebar: "Delete issue" (repo admins, not PRs) with
  confirmation, also in the command palette.
* Mock: `mock/extra/moderation.ts` (+ test) serves every endpoint and
  records edits by wrapping `server.put`.
* Playwright: `scripts/moderation-smoke.mjs` (mock mode) — edit history,
  hide / show / unhide, persistence across reload, delete issue.

## Tests

* `crates/bgh-issues/tests/it/moderation.rs`: hide/unhide (permissions,
  validation, sync data, audit), edit history of issue body + comments
  (three edits → three entries with editors; revision deletion rules;
  history dropped with the comment), review/commit comment history and
  hiding, issue deletion (403/401, 410, list/search/count, sync `D`,
  cleanup, audit, number reserved, PR → 422).
* `crates/bgh-graphql/tests/it/moderation.rs`: minimize/unminimize seen by
  another viewer, three edits → `userContentEdits` totalCount 3 with
  editors, comment history, `deleteIssue`, schema names.

## Gate

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D
warnings`, `cargo test --workspace` (59 binaries ok), web typecheck / lint /
test (462) / build (initial JS 144.4 KB gzip, unchanged by P42), Playwright
`scripts/moderation-smoke.mjs` all pass. The bgh-sync shape tests
(`tests/it/bootstrap.rs`) now expect the new fields.

## Known gaps

* `members_can_delete_issues` (P48) isn't honoured yet: only repo admins
  delete issues.
* No webhooks for minimizing (GitHub sends none either).
