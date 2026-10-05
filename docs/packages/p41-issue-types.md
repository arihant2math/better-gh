Integration: landed
P41: organization issue types, issue dependencies (blocked by / blocking), close as duplicate — backend, GraphQL, sync, web UI, tests.

# P41 — Issue types, issue dependencies, close as duplicate

Branch `bgh/p41-issue-types`. Migration `5300_issue_types_dependencies.sql`
(range 5300–5399).

## REST endpoints

| Method | Path | Notes |
|---|---|---|
| GET | `/orgs/{org}/issue-types` | Plain array (not paginated, like GitHub) of `issue-type` `{id, node_id, name, description, color, created_at, updated_at, is_enabled}`. 404 for unknown orgs and users. |
| POST | `/orgs/{org}/issue-types` | Org owners / site admins (`admin:org` for tokens); 404 non-members, 403 members. Body `{name, is_enabled (required), description, color, is_private (ignored)}` → **200** (GitHub answers 200). 422 `already_exists` (case-insensitive name), invalid `color` (gray, blue, green, yellow, orange, red, pink, purple, null), max 25 per org. |
| PUT | `/orgs/{org}/issue-types/{issue_type_id}` | Same body/validation → 200. Re-syncs every issue of the type. |
| DELETE | `/orgs/{org}/issue-types/{issue_type_id}` | 204; issues of the type lose it (`ON DELETE SET NULL`, re-synced). |
| GET | `/repos/{o}/{r}/issues/{n}/dependencies/blocked_by` | Issues blocking `n` (paginated, `Link`); unreadable ones dropped. |
| POST | `/repos/{o}/{r}/issues/{n}/dependencies/blocked_by` | `{issue_id}` → 201 with issue `n`. Triage on `n`'s repo; the blocker must be a readable issue (any repo, 422 otherwise, also for PRs). 422 for self, duplicates, **cycles** (recursive check under a global advisory xact lock) and > 50 per direction. |
| DELETE | `/repos/{o}/{r}/issues/{n}/dependencies/blocked_by/{issue_id}` | 200 with issue `n`; 404 if not blocked by it. |
| GET | `/repos/{o}/{r}/issues/{n}/dependencies/blocking` | Issues `n` blocks. |

Issue shape (all issue responses): new `type` (`issue-type` or `null`) and
`issue_dependencies_summary` `{blocked_by, blocking, total_blocked_by,
total_blocking}` (the non-`total` counts are open issues), like GitHub.

`POST /repos/{o}/{r}/issues`: `type` (name; triagers, silently dropped
otherwise like labels; 422 for unknown/disabled names or user-owned repos)
and the bgh extension `template` (template name or file name): the
template's `type:` applies for anyone unless `type` is given (used by the
web new-issue page and GraphQL `issueTemplate`).

`PATCH /repos/{o}/{r}/issues/{n}`: `type` (name, `null` clears) and the bgh
extension `duplicate_of` (issue id, readable, any repo; implies
`state: closed, state_reason: duplicate`; 422 with another `state_reason`,
with `state: open`, or pointing at itself). `state_reason: duplicate` is
kept everywhere (REST, sync — the old `sync/shapes.rs` downgrade to
`not_planned` is removed).

List filters: `GET …/issues?type=Bug|*|none` (repo and cross-repo lists).
Server search (`/search/issues`): `type:Bug` (any value other than
issue/pr is an issue type name), `no:type`, `is:blocked` (has an open
blocker), `is:blocking` (blocks an open issue).

## Timeline events (`issue_events`, for P45)

| event | on | `data` (REST timeline/events render the keys verbatim) | client `data` |
|---|---|---|---|
| `issue_type_added` | issue | `{issue_type: {id, name, color}}` | `issueTypeName`, `issueTypeColor` |
| `issue_type_changed` | issue | `{issue_type, prev_issue_type}` | + `prevIssueTypeName`, `prevIssueTypeColor` |
| `issue_type_removed` | issue | `{issue_type}` (the removed one) | as added |
| `blocked_by_added` / `blocked_by_removed` | blocked issue | `{blocking_issue: {id, number, repository: "o/r"}}` | `otherIssueId`, `otherIssueNumber`, `otherIssueRepository` |
| `blocking_added` / `blocking_removed` | blocking issue | `{blocked_issue: {id, number, repository}}` | same `otherIssue*` |
| `marked_as_duplicate` / `unmarked_as_duplicate` | duplicate | `{canonical: {id, number, repository}}` | same `otherIssue*` |
| `closed` (state_reason `duplicate`) | duplicate | `{state_reason: "duplicate", duplicate_of: {id, number, repository}}` (REST shows only `state_reason`) | `stateReason` + `otherIssue*` |

GraphQL mapping for P45: `IssueTypeAddedEvent/ChangedEvent/RemovedEvent`
(`issueType`, `prevIssueType` — resolve by `data.issue_type.id`),
`BlockedByAddedEvent/RemovedEvent` (`blockingIssue`),
`BlockingAddedEvent/RemovedEvent` (`blockedIssue`),
`MarkedAsDuplicateEvent/UnmarkedAsDuplicateEvent` (`canonical`,
`duplicate` = the event's issue). Reopening a duplicate clears it
(`unmarked_as_duplicate`), as does re-closing with another reason.

## GraphQL

`Issue.issueType` (`IssueType {id, name, description, color: IssueTypeColor, isEnabled}`,
node type `IssueType`) and `Issue.issueDependenciesSummary`, both batch
loaded (`Loaders.issue_types`, `Loaders.issue_deps`; `model/issue_type.rs`).
`createIssue(issueTypeId, issueTemplate)`, `updateIssue(issueTypeId)`
(`null` clears; now `MaybeUndefined`), `closeIssue(duplicateIssueId)`.

## Sync (docs/SYNC_PROTOCOL.md §3 updated)

Issue rows gain `issueType` (`{id, name, color}` or null), `duplicateOfId`,
`blockedByIds`, `openBlockedBy`, `blockingIds`. Closing/reopening an issue
re-syncs the issues it blocks / is blocked by (`dependencies::sync_dependents`
from `service::set_state_with`); renaming/recoloring/deleting a type
re-syncs its issues. No new synced model; `CLIENT_SCHEMA_VERSION` not
bumped (fields are optional; old local rows pick them up on their next delta).

## Tables

`issue_types(org_id, name, description, color, is_enabled)` (unique
`(org_id, lower(name))`; GitHub's Task/Bug/Feature seeded for existing orgs
and by an `AFTER INSERT` trigger on `users` for new orgs),
`issues.issue_type_id` / `issues.duplicate_of_id` (partial indexes),
`issue_dependencies(blocked_id, blocking_id)` (PK + `(blocking_id, blocked_id)` index).

## Web

* `sync/issueRelations.ts` (feature-local, lazy): mutations `setIssueType`,
  `closeAsDuplicate`, `addBlockedBy`/`addBlocking`/`removeBlockedBy`
  (optimistic), REST helpers for issue types.
* `pages/issues/IssueRelations.tsx` (+ `.module.css`): sidebar **Type**
  picker (org types via `useResource`) and **Relationships** (blocked by /
  blocking lists, add via menu + issue picker with local cycle filtering,
  remove; cross-repo rows fetched from the dependency endpoints), header
  tags (type chip, Blocked, "Closed as duplicate of #N"), `DuplicatePicker`.
* `Timeline.tsx` (P41 regions only, additive): event icons + cases for the
  events above, "closed this as a duplicate of #N", and "Close as duplicate"
  in the close menu.
* `IssueRow`: type chip and red "Blocked" badge. `filters.ts`: `type:`,
  `no:type`, `is:blocked`, `is:blocking` (+ qualifier autocomplete).
* New issue page sends the chosen template (`template`), so its `type:` applies.
* Org settings → **Issue types** (`/organizations/:org/settings/issue-types`,
  lazy route, nav `g y`): list, create/edit (name, description, color,
  enabled), delete.
* Mock: `mock/extra/relationships.ts` (types CRUD, `type`/`duplicate_of` on
  PATCH via an override route that falls through, dependencies).
* UI smoke: `node scripts/relationships-smoke.mjs [baseUrl]` (mock mode).

## Shared-code changes (additive)

* `bgh-core`: `NodeType::IssueType`; `sync/shapes.rs` issue columns +
  event data keys, `stateReason` downgrade removed.
* `bgh-search/src/issues.rs`: `type:<name>`, `no:type`, `is:blocked`, `is:blocking`.
* `bgh-graphql`: new `model/issue_type.rs`, two loader fields, `issue.rs`
  fields, `mutation/issues.rs` inputs.
* `bgh-sync/tests/it/bootstrap.rs`: issue shape assertion includes the new keys.

Deviation from the plan: `/orgs/{org}/issue-types` lives in `bgh-issues`
(`issue_types.rs`) rather than `bgh-accounts`, so the issue rendering, the
type CRUD and the re-sync of typed issues share one module without a
cross-crate dependency.

## Known gaps

* No `issues.typed` / `issues.untyped` webhook actions (no new `Event`
  variants), no `issue_dependencies` webhooks.
* The web relationship and duplicate pickers list issues of the same
  repository from the local store; cross-repo targets work through the API.
* `Organization.issueTypes` GraphQL connection not added.
