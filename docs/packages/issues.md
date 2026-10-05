# Package B3: issues (`bgh-issues`)

Status: **complete** (full B3 scope implemented, 25 integration tests and
7 unit tests passing). Branch `bgh/issues`.

## Endpoints

REST (relative to `/api/v3`, GitHub shapes, `page`/`per_page` + `Link`):

| Area | Endpoints |
|------|-----------|
| Issues | `GET/POST /repos/{o}/{r}/issues` (filters `milestone` (`*`/`none`/number), `state`, `assignee` (`*`/`none`/login), `creator`, `mentioned`, `labels`, `sort` created/updated/comments, `direction`, `since`), `GET/PATCH /repos/{o}/{r}/issues/{n}` (301 + `Location` for transferred issues), `GET /issues`, `GET /user/issues`, `GET /orgs/{org}/issues` (`filter` assigned/created/mentioned/subscribed/repos/all + the same filters; items embed `repository`) |
| Lock | `PUT/DELETE /repos/{o}/{r}/issues/{n}/lock` (`lock_reason` off-topic/too heated/resolved/spam) |
| Transfer | `POST /repos/{o}/{r}/issues/{n}/transfer` `{new_owner?, new_name}` → 201 issue at its new location |
| Comments | `GET /repos/{o}/{r}/issues/comments` (`sort`, `direction`, `since`), `GET/PATCH/DELETE /repos/{o}/{r}/issues/comments/{id}`, `GET/POST /repos/{o}/{r}/issues/{n}/comments` (`since`) |
| Reactions | `GET/POST /repos/{o}/{r}/issues/{n}/reactions`, `DELETE …/reactions/{id}`, `GET/POST /repos/{o}/{r}/issues/comments/{id}/reactions`, `DELETE …/reactions/{id}` (`content` filter; POST 201 new / 200 existing) |
| Events | `GET /repos/{o}/{r}/issues/events` (newest first, embeds `issue`), `GET /repos/{o}/{r}/issues/events/{id}`, `GET /repos/{o}/{r}/issues/{n}/events`, `GET /repos/{o}/{r}/issues/{n}/timeline` (events + `commented` comments + `cross-referenced` with `source.issue`) |
| Labels | `GET/POST /repos/{o}/{r}/labels`, `GET/PATCH/DELETE /repos/{o}/{r}/labels/{name}`, `GET/POST/PUT/DELETE /repos/{o}/{r}/issues/{n}/labels`, `DELETE /repos/{o}/{r}/issues/{n}/labels/{name}`, `GET /repos/{o}/{r}/milestones/{n}/labels` |
| Milestones | `GET/POST /repos/{o}/{r}/milestones` (`state`, `sort` due_on/completeness, `direction`), `GET/PATCH/DELETE /repos/{o}/{r}/milestones/{n}` |
| Assignees | `GET /repos/{o}/{r}/assignees`, `GET /repos/{o}/{r}/assignees/{u}`, `POST/DELETE /repos/{o}/{r}/issues/{n}/assignees`, `GET /repos/{o}/{r}/issues/{n}/assignees/{u}` |
| Sub-issues | `GET/POST /repos/{o}/{r}/issues/{n}/sub_issues` (`sub_issue_id`, `replace_parent`), `DELETE /repos/{o}/{r}/issues/{n}/sub_issue`, `PATCH /repos/{o}/{r}/issues/{n}/sub_issues/priority` (`after_id`/`before_id`), `GET /repos/{o}/{r}/issues/{n}/parent` |

Web client (`/_bgh`):

| Endpoint | Response |
|----------|----------|
| `GET /_bgh/repos/{o}/{r}/issue-templates[?ref=]` | `{commit_sha, templates: [{filename, type: "markdown"\|"form", name, about, title, labels, assignees, projects, issue_type, body, form}], config: {blank_issues_enabled, contact_links: [{name, url, about}]}, errors: [{filename, message}]}` — parsed from `.github/ISSUE_TEMPLATE/*.md|yml|yaml` + `config.yml` (legacy `ISSUE_TEMPLATE.md` fallback), forms validated (types, unique ids/labels, options), cached in Redis by commit SHA |
| `GET /_bgh/repos/{o}/{r}/pinned-issues` | array of issues (GitHub shape) |
| `PUT/DELETE /_bgh/repos/{o}/{r}/issues/{n}/pin` | 204 (max 3 per repo → 422; write permission) |
| `GET /_bgh/repos/{o}/{r}/issues/{n}/viewer-reactions` | `{issue: [content], comments: {"<id>": [content]}}` — the viewer's own reactions (added by issues-web) |
| `DELETE /_bgh/repos/{o}/{r}/issues/{n}/reactions/{content}`, `DELETE /_bgh/repos/{o}/{r}/issues/comments/{id}/reactions/{content}` | 204; delete the viewer's reaction by content (idempotent; added by issues-web) |

Media types: `application/vnd.github.{raw,text,html,full}+json` select
`body` / `body_text` / `body_html` on issues, comments and timeline comments
(`body_html` via `bgh_core::markdown` with repo context).

## Semantics

* Permissions: read to list/get; any authenticated reader can open issues and
  comment; labels/assignees/milestone on create/update need triage (silently
  dropped otherwise, like GitHub); close/reopen: author or triage; title/body:
  author or write; lock/unlock, issue labels, sub-issues: triage; label and
  milestone CRUD, pins, transfer: write; comment edit/delete: author or
  write; reaction delete: reactor or admin. Locked issues: comments and
  reactions need triage (403 "Unable to create comment because issue is
  locked."). Archived repos: writes 403; `has_issues = false`: 410 for issues
  (PR rows stay reachable). No read access → 404 everywhere.
* Issue numbers are shared with PRs (`repositories.next_issue_number`); PR
  rows appear in issue lists with `pull_request` and `draft`.
* Counters in the same transaction: `issues.comments_count`,
  `repositories.open_issues_count` (issues + PRs, like GitHub), milestone
  `open_issues`/`closed_issues` (recomputed for affected milestones).
* Default labels (GitHub's 9, `bgh_core::labels::DEFAULT_LABELS`) are
  created by bgh-repos inside the repository-creation transaction (not for
  forks); adding unknown labels to an issue creates them (like GitHub).
* Mentions/cross-references (`bgh_core::markdown::extract_references`,
  ignores code): on create/edit of bodies and comments, only newly added
  references count. Mentioned users who can read the repo get
  `issue_mentions`, `mentioned` + `subscribed` events, a thread subscription
  and `Event::IssueMentioned`. Referenced issues (same repo, `owner/repo#n`,
  or full URLs) the actor can read get one `cross-referenced` event per
  source issue + `Event::IssueCrossReferenced`; the timeline hides sources
  the viewer can't read.
* Pushes (`issues.commit_references` listener on `Event::Push`): commits
  referencing issues add `referenced` events (deduped by commit); closing
  keywords (`fixes #n` …) on the default branch close the issue with
  `commit_id`.
* Transfer: same owner only, write on both repos, private→public refused;
  comments/events/reactions move, labels and milestone kept when a same-named
  one exists in the target, assignees kept when assignable, pin dropped,
  `transferred` event, old `(repo, number)` answers 301.
* Thread subscriptions (`thread_subscriptions`, subject type `Issue` /
  `PullRequest`) are inserted with `ON CONFLICT DO NOTHING` for authors,
  commenters, assignees and mentioned users (reasons `author`, `comment`,
  `assign`, `mention`); they back `filter=subscribed`.

## Sync

Every write records sync actions in `repo:{id}` through the shared shape
helpers (`tx.sync_model` / `sync_issue` / `sync_models` / `sync_delete`,
BACKEND_PATTERNS.md §8a), so deltas equal the bootstrap / partial-sync rows:
`issue` (lazy `body` only on insert / body edits), `label`, `milestone`,
`comment`, `issueEvent`, and the `repo` row when its open counts change.
Reactions have no model of their own: the reacted issue/comment row is
re-synced. Label / milestone deletes re-sync the affected issues, then
record the delete. Transfers send `D` in the old scope and `I` (issue +
comments) in the new one.

Protocol extensions made here (SYNC_PROTOCOL.md §3 / §3.2,
`web/src/sync/models.ts`): `Issue.activeLockReason`, `Issue.parentId`,
`Issue.subIssueIds` (ordered children; the parent is re-synced on
add/remove/reorder), `Issue.pinned`; extra `IssueEvent` types (`mentioned`,
`subscribed`, `cross-referenced`, `pinned`, `unpinned`, `transferred`,
`sub_issue_*`, `parent_issue_*`) and data keys (`lockReason`,
`sourceIssueId`, `sourceCommentId`, `sourceNumber`, `sourceRepository`,
`sourceIsPr` (stored on new `cross-referenced` events), `subIssueId`,
`subIssueNumber`, `subIssueRepository`, `parentIssueId`, `parentIssueNumber`,
`parentIssueRepository`, `fromRepository`), all rendered by
`bgh_core::sync::shapes`. `stateReason` `duplicate` is sent as
`not_planned` (protocol server note; the client also accepts `duplicate`).

## Database (migration `0300_issues.sql`)

New tables: `issue_mentions(issue_id, user_id)`, `sub_issues(child_id PK,
parent_id, position)`, `pinned_issues(issue_id PK, repo_id, position,
pinned_by_id)`, `issue_transfers(old_repo_id, old_number) → issue_id`.
Indexes for list sorts/filters: issues `(repo_id, state, created_at, id)`,
`(repo_id, state, comments_count, id)`, `(repo_id, updated_at, id)`,
comments `(repo_id, created_at, id)`, events `(issue_id, event)`, plus FK
indexes. Event/comment/assignee rows use `clock_timestamp()` so items
written in one transaction keep their order.

## Shared-code changes (additive)

* `bgh_core::events::Event`: `IssueLabeled`, `IssueUnlabeled`,
  `IssueAssigned`, `IssueUnassigned`, `IssueMilestoned`, `IssueDemilestoned`,
  `IssueLocked`, `IssueUnlocked`, `IssuePinned`, `IssueUnpinned`,
  `IssueTransferred`, `IssueMentioned`, `IssueCrossReferenced`,
  `IssueReferenced`, `SubIssueAdded`, `SubIssueRemoved`, `LabelCreated`,
  `LabelEdited`, `LabelDeleted`, `MilestoneCreated`, `MilestoneEdited`,
  `MilestoneClosed`, `MilestoneOpened`, `MilestoneDeleted`,
  `ReactionCreated`, `ReactionDeleted`. Closing/reopening a PR through the
  issues API emits `PullRequestClosed`/`PullRequestReopened`.
* `bgh_core::markdown::{extract_references, References, IssueRef}` (also
  usable by bgh-notify for mention parsing).
* Workspace dependency `serde_yaml` (issue forms).

## For other packages

`bgh_issues::service` exposes transaction-level building blocks that keep
counters, events and sync consistent, e.g. for bgh-pulls:
`allocate_number`, `set_state`, `add_labels`/`remove_labels`/`replace_labels`,
`add_assignees`/`remove_assignees`, `set_milestone`, `add_event`,
`touch_and_sync`, `subscribe`, `refresh_milestones`; and
`bgh_issues::refs::process` for mentions/cross-references in PR bodies.
`bgh_issues::json::{issues, comments, events}` render the GitHub shapes in
batch.

## Known gaps / TODO

* Team mentions (`@org/team`) are extracted but not acted on (notify).
* Event `node_id`s all use the `IssueEvent` type (GitHub uses per-event
  types such as `LabeledEvent`).
* 422 `errors[]` entries lack GitHub's `value` key (core `FieldError` has no
  such field).
* `author_association` never reports `FIRST_TIMER` /
  `FIRST_TIME_CONTRIBUTOR`.
* `X-GitHub-Media-Type` always reports `format=json` (set by the server
  middleware).
* The commit-reference listener is in-process/best-effort (no job).
* No REST issue deletion (GitHub only offers it in GraphQL).
