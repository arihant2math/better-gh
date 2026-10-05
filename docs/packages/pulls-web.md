# F4 pulls-web — status

Branch `bgh/pulls-web` (web client + small additive backend endpoints in
`bgh-pulls`). Builds on `bgh/pulls`, `bgh/sync`, `bgh/git-transport` (merged
into this branch); compare / commit / contents / branches / forks endpoints
come from `bgh/repos-api` (used through their GitHub REST shapes; not merged
here — see "Integration notes").

**Status: scope complete** — verified in mock mode (Playwright smoke +
unit tests) and against the real backend (see "Verification").

## What's implemented (web)

Pull request page `/{o}/{r}/pull/{n}[/{tab}]` (`pages/pulls/PullDetailPage.tsx`),
heavy tabs are lazy chunks prefetched on link intent:

* **Files changed** (`FilesTab.tsx`, `components/diff/DiffView.tsx`)
  * Virtualized diff (one row per line/pair, measured), file tree with comment
    counts and status dots, text filter (`t`), "Hide viewed".
  * Changed files loaded page by page (`GET /pulls/{n}/files`, 100 per page,
    immutable per base/head SHA) as you scroll; files over 1500 changed lines
    or with patches omitted by the server render "Load diff" and fetch a
    single-file patch lazily (`GET /_bgh/.../pulls/{n}/patch?path=`).
  * Unified / split toggle (`s`, `?diff=split`, remembered), hide whitespace
    (`w`, `?w=1` → per-file patches with `-w`, fetched as files scroll in).
  * Viewed checkboxes (`v`) persisted per PR in localStorage keyed by blob
    SHA (a changed file becomes unviewed again, like GitHub); viewed files
    start collapsed; `x` collapses.
  * Inline comments: click/drag the gutter (or shift-click) for single or
    multi-line ranges on either side (`LEFT`/`RIGHT`), `c` comments on the
    keyboard cursor line (`j`/`k` lines, `n`/`p` files), file-level comments
    (header button, `subject_type: file`). Composer: "Add single comment" or
    "Start a review" / "Add review comment", "Suggest" inserts a
    ```suggestion block with the selected lines.
  * Threads anchored under their line (both views), replies, edit/delete,
    reactions, resolve / unresolve (resolved and outdated threads collapsed;
    outdated + file threads under the file header).
  * Suggested changes render as a diff (current lines from the loaded patch,
    or the comment's `diffHunk`); "Commit suggestion" rewrites the lines on
    the head branch through the contents API (`GET`/`PUT /contents/{path}`).
  * Pending review flow: server-side pending reviews (survive reloads and
    devices); "Finish your review" (`shift+r`) → Comment / Approve / Request
    changes with summary (own PR: approve/request disabled), discard.
* **Conversation**: timeline (F3's `Timeline`, extended with an additive
  `renderReview` slot) shows each review's inline threads with file path +
  diff excerpt; sidebar "Reviewers" (latest state per reviewer, pending
  requests, re-request, user + team picker `shift+q`); merge box
  (`MergeBox.tsx`): review / checks rows (expandable check list), blockers
  from `/_bgh/.../requirements`, conflicts, out-of-date → "Update branch",
  merge method split button (remembered per repo) with confirm step and
  editable commit title/message, auto-merge enable/disable, draft ⇄ ready,
  merged state with "Delete branch", closed state.
* **Checks** (`ChecksTab.tsx`): suites → runs (latest per name) + commit
  statuses, rollup summary, run detail with duration, annotations
  (`/check-runs/{id}/annotations`), re-run; `j`/`k`. Rollup icon also in
  the PR header and on the head commit; PR lists already show `issue.checks`.
* **Commits** (`CommitsTab.tsx`): grouped by day; `/pull/{n}/commits/{sha}`
  shows the commit's diff (`.diff` of `/commits/{sha}`), prev/next (`[`/`]`).
* **Compare & new PR** (`ComparePage.tsx`, routes `/{o}/{r}/compare[/{base}...{head}]`):
  base/head branch pickers, head repository picker (forks via `/forks`,
  `owner:branch` heads), ahead/behind status, existing-PR notice, commit
  list + diff preview (from the compare `files`), "Create pull request" form
  (`?expand=1`) with title from the single commit / branch name, body from
  `.github/pull_request_template.md` (and the usual fallbacks), draft option
  (split button), `mod+enter`. "New pull request" button + `c` on the PR list.
* `g c` / `g m` / `g k` / `g f` switch PR tabs. Every write is optimistic
  through the sync store.

## Sync / data layer

* New lazy models in `web/src/sync/models.ts` + `schema.ts` (client schema
  version 2): `reviewComment`, `reaction`, `checkSuite`, `checkRun`,
  `commitStatus` (shapes = bgh-pulls' sync rows, documented in
  SYNC_PROTOCOL.md §3.0); PR `issue` extensions (`autoMerge`, …).
* `SyncClient.loadPull(issueId)` / `usePullDetails` load them per PR and per
  head SHA from `GET /_bgh/repos/{o}/{r}/pulls/{n}/sync`; deltas keep them live.
* `TxQueue.commit({ apply })` (`TxApply`): for writes the server doesn't
  broadcast (pending review comments, discards) the response rows are moved
  into the base store before the overlay is dropped. `ObjectPool.removeRows`.
* Selectors: `sync/pullSelectors.ts` (threads, pending review, latest
  reviews, reviewer candidates, checks rollups). Mutations:
  `sync/pullMutations.ts` (comments, replies, resolve, pending review,
  submit/discard/dismiss, reviewers, merge with method/title, auto-merge,
  update branch, delete head branch, base change, close/reopen, reactions,
  create PR, commit suggestion).
* `overlay.ts`: `Patch` array patches now also type-check on optional array fields.
* Mock backend: `src/mock/pulls.ts` implements every endpoint above
  (threads/checks seeded lazily per PR); mock state version 4.

## Backend additions (bgh-pulls, additive — see docs/packages/pulls.md)

* `GET /_bgh/repos/{o}/{r}/pulls/{n}/sync` — PR extension rows snapshot.
* `POST /_bgh/repos/{o}/{r}/pulls/{n}/reviews/pending/comments` — add a
  comment / reply to the viewer's pending review (GraphQL
  `addPullRequestReviewThread` equivalent), creating the review if needed.
* `GET /_bgh/repos/{o}/{r}/pulls/{n}/patch?path=&w=1` — one file's patch,
  optionally ignoring whitespace (`bgh_git::patch::diff_file`).
* `reviewComment` sync rows gained `diffHunk`.
* Tests: `crates/bgh-pulls/tests/web_client.rs`.

## Tests & verification

* `npm run typecheck && npm run lint && npm test && npm run build` green
  (bundle: initial JS ~120 KB gzip; FilesTab/DiffView/ReviewThread chunks
  ≤ 20 KB each).
* `web/src/sync/pulls.test.ts`: patch parsing, split pairing, anchored rows,
  suggestion parsing, threads/checks loading, single comment + reply +
  resolve, pending review start/add/submit/discard, rollback.
* `web/scripts/pulls-smoke.mjs` (Playwright): drag-select range comment →
  pending review, keyboard `j`/`c` comment, split + whitespace toggles,
  viewed persistence across reload, submit review, conversation/checks/
  commits/per-commit diff, compare → draft PR. Mock mode by default,
  `--real --pr /o/r/pull/n` against a server.
* `cargo test -p bgh-pulls` (incl. `web_client.rs`), clippy clean.

## Integration notes / known gaps

* Needs `bgh/repos-api` for compare, `/commits/{sha}`, branches, forks,
  contents (PR template, commit suggestion) and `DELETE /git/refs` (delete
  branch); `bgh/accounts` for sign-in. Not merged here (many conflicts
  outside this package); they work through GitHub shapes once integrated.
* Viewed-file state is per browser (localStorage), not server-side.
* Checks/statuses are synced for the head commit only; older commits in the
  Commits tab have no status icon.
* Syntax highlighting in diffs is not implemented (plain text).
* Timeline rendering of PR-only events (`review_dismissed`,
  `auto_merge_enabled`, `base_ref_changed`, …) uses the generic fallback of
  F3's `EventItem`; F3 owns that component.
* The `MarkdownEditor` is imported from F3's `pages/issues/Timeline.tsx`;
  move both call sites if F3 relocates it.
