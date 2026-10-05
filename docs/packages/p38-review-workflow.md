Integration: landed
Commit-range diffs + "since your last review", server-side viewed files (synced `viewedFile`), batch suggestion commits (server-side, CRLF-safe, co-author trailers, auto-resolve); backend + web + tests, full gate green after merging the integration branch.

# P38 review-workflow — status

Scope: `docs/PHASE4_PLAN.md` §P38 (no §5 quick fixes are assigned to P38).
Branch `bgh/p38-review-workflow`. Evidence: `docs/AUDIT.md` (pulls: "Files
tab can't filter by commit…", "Suggestions are applied one by one…",
"'Viewed' file state is per-browser only").

## Endpoints (bgh-pulls, all `/_bgh/repos/{o}/{r}/pulls/{n}/…`)

| Endpoint | Module | Notes |
|---|---|---|
| `GET /files?base_sha=&head_sha=&page=&per_page=` | `ranges.rs` | REST `/pulls/{n}/files` entry shape, `Link` with `last`. No params = the PR diff (merge base → head). Only `head_sha` = merge base → that commit. Only `base_sha` = that commit → PR head ("changes since your last review" passes the review's `commit_id`). Malformed SHA → 422 with `errors[{field}]`; unknown commit → 422 `No commit found for SHA: …`. With both SHAs: `Cache-Control: private, max-age=31536000, immutable`. Diffs come from the Redis-cached `git::diff` keyed by SHAs |
| `GET /patch?path=&w=&base_sha=&head_sha=` | `web.rs` | existing single-file patch, now range-aware (same resolution as `/files`) |
| `GET /viewed` | `viewed.rs` | viewer's marked files still in the PR diff: `[{path, blob_sha, state: VIEWED\|DISMISSED}]` (401 anonymous) |
| `PUT /viewed {path, blob_sha?}` | `viewed.rs` | upsert; the path must be in the current PR diff (422, so rows stay bounded by the diff); `blob_sha` defaults to its blob there (422 for a malformed SHA; 422 without `path`) → `{path, blob_sha, state}` |
| `DELETE /viewed?path=` | `viewed.rs` | 204, idempotent; 422 without `path` |
| `POST /suggestions/apply {comment_ids, message?, description?, expected_head_sha?}` | `suggestions.rs` | 201 `{commit_sha, resolved_thread_ids}`. One commit on the head branch for 1–100 suggestions (see below). 401 anonymous, 403 without push access (write on the head repo, or write on base + `maintainer_can_modify`), 422 for: empty list, closed PR, pending/deleted/foreign comments, comments without a ```suggestion block, outdated or LEFT-side comments, overlapping line ranges, binary or missing files, a head branch that moved (`expected_head_sha` mismatch or branch tip ≠ PR head) |

### Suggestion commits

* Files are read at the PR head with no size limit beyond git's
  (`blob_with_limit(u64::MAX)`), edited in memory (`suggestions::apply`) and
  written with `GitCli::build_tree` + `commit_tree` (file mode preserved).
* Line endings: each replacement line takes the terminator of the first
  replaced line; the last one takes the terminator of the last replaced line
  (so CRLF files stay CRLF and a missing final newline stays missing).
  Suggestion text is CRLF-normalized; an empty block deletes the lines.
* Message: headline (`message`, default "Apply suggestion(s) from code
  review"), blank line, `description`, blank line, one
  `Co-authored-by: Name <email>` per distinct suggestion author other than
  the applier (email = `git::identity`: verified primary or the
  `{id}+{login}@users.noreply.{host}` address). Author = applier, committer
  = site identity (like merges).
* The branch moves through `bgh_repos::refs::write_ref` (archived/mirror
  checks, branch protection and rulesets, post-receive → `Event::Push` →
  PR synchronize). `.github/workflows/*` paths need the `workflow` scope
  (`workflow_scope::check_path`).
* The applied threads (roots) are resolved by the applier in one
  transaction: `reviewComment` sync updates, `Refresh` job,
  `PullRequestReviewThreadResolved` events (→ `pull_request_review_thread`
  webhooks).

## Data model (for P45 GraphQL)

Migration `migrations/5000_pull_viewed_files.sql` (range 5000–5099):

```
pull_viewed_files(id, pull_id → pull_requests ON DELETE CASCADE,
                  repo_id → repositories ON DELETE CASCADE,
                  user_id → users ON DELETE CASCADE,
                  path, blob_sha, created_at, updated_at)
UNIQUE (pull_id, user_id, path); INDEX (user_id, pull_id); INDEX (repo_id)
```

`blob_sha` is the REST diff entry `sha` at the time of marking
(`viewed::entry_sha`: new blob, else old blob for removed files). Service
functions for GraphQL (`bgh_pulls::viewed`):

* `mark(state, access, pull, user_id, path, blob_sha: Option<&str>)` →
  `markFileAsViewed` (pass `None`: the current PR diff's blob).
* `unmark(state, pull, user_id, path)` → `unmarkFileAsViewed`.
* `states(state, pull, user_id) -> HashMap<path, ViewedState>` →
  `PullRequestChangedFile.viewerViewedState` (`VIEWED`, `DISMISSED` = marked
  but the blob changed, `UNVIEWED`; serializes in GitHub's
  SCREAMING_SNAKE_CASE).
* Suggestions: `suggestions::apply_batch(state, auth, access, pull, &ApplyBody)`
  (no GraphQL equivalent on GitHub; the web UI only).
* Ranges: `ranges::resolve_range(state, pull, &RangeQuery)` → `(base, head)`.

## Sync

New extension model `viewedFile` (`bgh_core::sync::shapes::Model::ViewedFile`,
additive): `{id, repoId, issueId, userId, path, blobSha, updatedAt}` in the
owner's `user:{id}` scope; a delta-only model (not in bootstrap/partial),
loaded for the viewer by `GET …/pulls/{n}/sync` (`models.viewedFile`; with
a viewer the shape loader filters rows to that viewer, without one — when
recording a delta — it loads any row). Unmark records `D`.
`docs/SYNC_PROTOCOL.md` §3.0/§3.2 updated; `bgh-sync` shapes test extended.

## Web (`web/src/pages/pulls`, lazy PR chunk)

* `range.ts` + `CommitRangePicker.tsx`: Files-tab picker — "Show all
  changes", "Show changes since your last review" (head at the viewer's
  latest submitted review, `lastReviewCommit`), one commit, or a range
  (shift-click). State in `?range=` (`review`, `<sha>`, `<sha>..<sha>`).
  `reviewApi.ts` calls the range endpoints. In a range view only RIGHT-side
  and file threads show and LEFT-side selection is off (the left is the
  range base); a range ending before the head hides threads and disables
  commenting (banner with "Show all changes").
* `viewed.ts`: reads `viewedFile` rows from the store, toggles through
  `setFileViewed` (optimistic insert/update/delete + PUT/DELETE). The old
  localStorage state (`bgh:viewed:<id>`) is migrated once (files whose blob
  still matches), when the whole PR diff is loaded, then removed.
* Suggestions: `suggestionBatch.ts` (in-memory batch per PR),
  `CommitSuggestionsDialog.tsx` ("Commit suggestion" / "Commit suggestions
  (N)" with editable message + description, co-author preview);
  `ReviewThread.tsx` shows "Commit suggestion" and "Add suggestion to batch"
  / "Remove from batch" (hidden for pending, outdated, LEFT-side or resolved
  suggestions). `applySuggestions` in `sync/pullMutations.ts` replaces the
  old client-side contents-API `commitSuggestion`.
* `sync/models.ts`/`schema.ts`: `ViewedFile` model (lazy, `user:` scope,
  cascades with its issue); `CLIENT_SCHEMA_VERSION` 2 → 3 (new IndexedDB
  store).
* Mock (`src/mock/pulls.ts`, `server.ts`): range `/files` (deterministic
  subset), `/viewed` PUT/DELETE, `viewedFile` in `/sync`,
  `/suggestions/apply`; PR commits now end at the PR head and carry
  `parents`.
* Bundle: initial JS −0.5 KB gzip (142.7 vs 143.2 before this package; 143.6
  after merging the current integration branch): the client-side suggestion
  code moved to the server; all new UI ships in the PR chunk.

## Tests

* `crates/bgh-pulls/tests/it/review_flow.rs`: range diffs (one commit,
  since last review, head-only, pagination `Link`, errors, cache header,
  ranged `/patch`, private repo 404), viewed state (persist, per user, sync
  delta = `/sync` row, DISMISSED after a change, unmark `D`), batch of 3
  suggestions → 1 commit with trailers and CRLF intact, threads resolved and
  synced, PR synchronized; single-suggestion default message, overlap,
  pending, stale head.
* `suggestions.rs` unit tests (CRLF, missing final newline, deletion,
  overlap, trailers).
* `crates/bgh-sync/tests/it/shapes.rs`: covers `viewedFile`.
* `web/src/pages/pulls/review.test.ts`: range parsing/resolution/toggling,
  last review, batch store, mock viewed + suggestions flow.
* Playwright smoke against a real `bgh serve` + built web (two users, CRLF
  file, review then push): all changes / since last review / one commit
  show the right files; viewed state shows in a second browser context and
  resets after the file changes; two batched suggestions → one commit
  `Apply review suggestions` + description + `Co-authored-by: bob
  <bob@example.com>`, CRLF intact, threads resolved.

## Shared-code changes (additive)

* `bgh-core/src/sync/shapes.rs`: `Model::ViewedFile` (+ `ALL` 17 → 18).
* `web/src/api/types.ts`: `RestCommit.parents?`.
* `web/src/sync/models.ts`, `schema.ts` (model + version bump).

## Known gaps / TODOs

* Commenting inside a commit-range view is limited to RIGHT-side lines of
  ranges ending at the head (comments are positioned on the full PR diff,
  like before); GitHub also allows comments on a single older commit.
* "Since your last review" uses the review's `commit_id` (the head when the
  review was started), not a separately tracked "last seen" head.
* After a force-push the reviewed commit may not be an ancestor of the new
  head; the range is then a plain two-tree diff (GitHub shows a similar
  compare).
* GraphQL fields/mutations are P45's (service functions above).
