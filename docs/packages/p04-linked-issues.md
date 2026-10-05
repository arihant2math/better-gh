# P4 — Closing keywords, linked issues/PRs and the Development section

Status: **done** (branch `bgh/p04-linked-issues`, self-integrated into
`claude/sleepy-cray-9jj0t3`).

## What it does

* **Links table** `issue_pr_links(issue_id, pull_id, source keyword|manual,
  created_by, created_at)`, PK `(issue_id, pull_id)` (migration
  `1600_issue_pr_links.sql`, indexes on `pull_id`, `created_by`).
* **Closing keywords** (`bgh_issues::refs::closing_issue_refs`): close(s|d),
  fix(es|ed), resolve(s|d), case-insensitive, optional `:`; targets `#N`,
  `owner/repo#N`, `{BGH_BASE_URL}/owner/repo/issues/N`. Fenced code and inline
  code are ignored. `closing_refs` (push commit messages) now uses the same
  parser (same-repo numbers only, unchanged behaviour).
* **Reconciliation** (`bgh_issues::links`, listener `issues.pr_links`): on
  `PullRequestOpened` and `PullRequestEdited` with a `body` change, the
  keyword links of the (unmerged) PR are brought in line with its body.
  Same-repo issues always link; cross-repo issues only when the PR author has
  triage on the target (and can read it). Manual links are never touched.
  Each added/removed link writes `connected` / `disconnected` events on
  **both** the issue and the PR, re-syncs both rows and emits
  `Event::IssueConnected` / `Event::IssueDisconnected`.
* **Closing on merge**: on `PullRequestMerged`, after a final reconciliation,
  if the PR's base is its repository's default branch every linked open issue
  is closed (`state_reason=completed`) through `service::set_state_with`: a
  `closed` event with `commit_id` = merge sha (+ `commit_repository` when
  cross-repo) and the PR as `source_*` data ("closed this as completed in
  #N"), `IssueClosed` (webhooks, notifications, Actions), counters, milestone
  and repo re-sync.
* **Privacy**: a source in another, non-public repository is stored
  anonymously on the target side (no `source_*`, no commit), the links API
  filters items the viewer can't read, and REST `connected`/`disconnected`
  events carry no extra fields (GitHub's shape).
* **Manual link API** (`/_bgh/repos/{o}/{r}/issues/{n}/links`, GET/POST and
  `DELETE …/links/{linked_id}`): see `docs/packages/issues.md`. Write access
  on both sides; only manual links can be deleted (keyword → 422).
* **GraphQL**: `PullRequest.closingIssuesReferences` and
  `Issue.closedByPullRequestsReferences` (`includeClosedPrs` honoured) read
  the table through batch loaders (`ClosingIssuesLoader`,
  `ClosedByPullsLoader`) filtered by the viewer's read permission. The old
  body-regex heuristics (`closing_numbers`) are gone.
* **Sync**: `issue` rows carry `linkedPullIds` (all rows) and PR rows
  `closingIssueIds` (SYNC_PROTOCOL.md §3/§3.2).
* **Web**:
  * `pages/issues/DevelopmentSection.tsx` in the shared sidebar: linked PRs
    (issue) or "Successfully merging this pull request may close these
    issues" (PR), state icons from the live store, linked branches, and a
    picker (same-repo PRs/issues) that links/unlinks optimistically
    (`setIssueLink`).
  * Issue list rows: PR icon + count (`data-testid="linked-prs"`).
  * Timeline: `connected` / `disconnected` and closed-by-PR renderers.
  * Mock backend: links endpoints and closing linked issues on merge.

## Tests

* `crates/bgh-issues/tests/it/links.rs`: the acceptance PR body
  (`Fixes #1, closes bob/lib#3, resolves {url}` + an untriaged cross-repo ref)
  linked, synced (delta = bootstrap), GraphQL, merged into `main` → all three
  closed with merge-sha `closed` events and `IssueClosed`; non-default base
  closes nothing; body edit → `disconnected`; manual links (201/200/422/404/
  403/401, keyword delete 422, closes on merge, unlink from the PR side);
  private-source anonymity.
* Unit tests for the keyword parser (`refs.rs`).
* `scripts/gh-compat.sh`: the fixture PR body says `Fixes #<fixture issue>`,
  new case `gh pr view --json closingIssuesReferences`.
* UI smoke (Playwright, real server serving `web/dist`): Development section
  shows the keyword link, drops it live when the PR body changes (sync, no
  reload), picker creates a manual link, PR sidebar lists both issues, list
  indicator, merge closes both with "closed this as completed in #3".

## Shared-code changes (additive)

* `bgh-core/src/events.rs`: `Event::IssueConnected` / `IssueDisconnected`
  (+ `name`, `repo_id`, `actor_id` arms).
* `bgh-core/src/sync/shapes.rs`: `linkedPullIds`, `closingIssueIds`.
* `bgh-issues/src/service.rs`: `set_state_with` (extra event data);
  `set_state` delegates to it.

## Known gaps

* No GitHub webhook exists for connected/disconnected, so the new events have
  no payload mapping (P10 may map them if wanted); closing still fires
  `issues.closed` via `IssueClosed`.
* The picker only lists PRs/issues of the same repository (cross-repo
  manual links work through the API).
* "Create a branch" for an issue (GitHub's Development action) is not in
  the UI; linked branches are shown (`{n}-…` naming, as `gh issue develop`).
* Reconciliation runs in an in-process listener (best effort until P9's
  durable delivery); the merge path reconciles again before closing.
* `linkedPullIds` may include ids of PRs the viewer can't read (only ids;
  the list indicator counts them).
