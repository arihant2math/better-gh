# F5 inbox-search-web — status

Branch `bgh/inbox-search-web`. Web client: inbox, dashboard, global search.
Small additive backend changes in `bgh-search` and `bgh-notify` (below).

## Status

Complete: scope implemented, web checks green, backend crate tests green,
real-backend Playwright e2e (`web/scripts/inbox-e2e.sh`) passes all checks.
Full `cargo test --workspace`: green except bgh-accounts
`rate_limits_are_enforced` (known integration conflict, being fixed on the
integration branch).

## Inbox (`/notifications`, `src/pages/notifications/`)

* **List + split preview** (`NotificationsPage`, `InboxRow`, `InboxPreview`):
  virtualized, grouped by **date** (Today / Yesterday / This week / This
  month / Older), **repository** (header shows the watch level and opens the
  watch dialog), **reason**, or none. The preview renders the issue/PR from
  the local store (body + latest 3 comments, "New" since `lastReadAt`,
  review chips, labels, branch); lazy parts load via partial sync, and a
  subject whose repo isn't synced pulls the `repo:{id}` scope in
  (`ensureScope`).
* **Filters** in the URL (`?unread=1&participating=1&reason=a,b&repo=o/r&type=issue|pr&group=…`):
  Unread, Participating (assign/author/comment/mention/review_requested/
  state_change/team_mention), Issues / Pull requests, repository picker
  (multi), reason chips with live counts. Pure logic in `inbox.ts` (tested).
* **Views**: built-ins (Inbox, Unread, Participating, Review requests,
  Mentions, Assigned) + custom saved views (`Save view` → name; stored per
  user in `localStorage` `bgh.inbox.views.{userId}`; deletable). Also in the
  ⌘K palette as "Inbox view: …".
* **Keyboard triage**: `j/k` (↑/↓) move, `enter`/`o` open (marks read),
  `e` done, `u` read/unread toggle, `s` subscribe/unsubscribe thread,
  `shift+i` mark all (visible) read, `x` / `shift+x` select (range with
  shift-click), `mod+a` select all, `esc` clear, `w` watch settings for the
  row's repo, `g u` toggle unread-only. With a selection, `e/u/s/shift+i`
  apply to it (bulk bar with buttons too).
* **Mutations** (`actions.ts`, optimistic through the tx queue):
  done = `DELETE /notifications/threads/{id}` (overlay delete; server syncs
  a delete), read = `PATCH /notifications/threads/{id}`, unread =
  `DELETE /_bgh/notifications/threads/{id}/read`, mark all = one
  `PUT /notifications` (whole inbox) / `PUT /repos/{o}/{r}/notifications`
  (single-repo filter) / per-thread PATCHes (other filters, selections).
  Thread subscriptions aren't synced: `GET/PUT/DELETE
  /notifications/threads/{id}/subscription` with an observable cache,
  optimistic + rollback toast.
* **Real-time arrival** (`src/app/unread.ts`, initial bundle, started by the
  Shell): a MobX reaction over unread notifications detects new threads /
  newer `updatedAt` from sync deltas (own optimistic "mark unread" is
  ignored), marks them for a 900 ms arrival animation, keeps the tab title
  prefix `(N)` (re-applied when the router rewrites the title) and draws an
  unread badge on the favicon (canvas).
* **Desktop notifications**: opt-in bell toggle in the inbox header
  (`Notification.requestPermission()` from the click; preference in
  `localStorage` `bgh.desktopNotifications`). Fired only when the tab is
  hidden/unfocused; > 3 arrivals at once collapse into one summary;
  clicking focuses the tab and opens `/notifications?id=…`.
* **Watch settings dialog** (`WatchDialog.tsx`, lazy; ⌘K "Watch settings for
  this repository…", `w` in the inbox, repo group headers):
  Participating and @mentions / All activity / Ignore / Custom (issues,
  pulls, releases, discussions, security alerts). Loads/saves
  `GET/PUT /_bgh/repos/{o}/{r}/subscription`; optimistic `viewerRepo.watching`
  + `repo.watchers` overlay.

## Dashboard (`/`, `src/pages/dashboard/`)

* **Activity feed** from `GET /_bgh/feed?before=&limit=40[&org=]`
  (`feed.ts`: per-context MobX store, refresh merges new events on top,
  cursor pagination). Virtualized (`VirtualList`, the greeting/work panel is
  its header) with **infinite scroll** (the last row triggers `loadMore`),
  day headers, and **grouping** of bursts (same actor + repo + type +
  action within 3 h: "pushed 5 commits (2 pushes)", "opened 4 issues",
  "commented 7 times"). Renderers for Push, Create/Delete, Issues,
  PullRequest (merged/closed/opened), IssueComment, PullRequestReview(+Comment),
  Watch, Fork, Release, Member, Public events.
* **Your work** panel from the local store with counts: Assigned, Review
  requests, Created, Mentioned (open subjects of mention/team_mention
  notifications). Tab remembered in `localStorage`.
* **Context switcher** (`?ctx=login`): All activity / yourself / each org;
  filters the feed (`org=`), the work panel, counts, unread card and recent
  repositories.
* Recent repositories (pushed-at order, stars), unread inbox card.

## Global search

* **Command palette** (`src/app/CommandPalette.tsx`, initial bundle): local
  results first (commands, repos, issues, people from the store), then
  server results from `GET /_bgh/search` streamed in below the local ones
  of the same group (deduped by id, subtle fade-in). `usePaletteSearch`:
  60 ms debounce, an `AbortController` per keystroke, LRU cache (120
  entries, 60 s) answering repeated queries synchronously, previous results
  kept while typing ahead (prefix) to avoid flicker. **Scopes**: Everywhere
  / org (owner of the current repo or profile page) / repo — `Tab` /
  `shift+Tab` cycle, `Backspace` on an empty input resets; local results are
  filtered too; the server gets `repo=` / `org=`. Last row: "Search for “q”"
  → `/search` with the scope as a qualifier.
* **Search page** `/search?q=&type=&p=&s=` (`src/pages/search/`, lazy):
  tabs Code / Repositories / Issues / Pull requests / Users / Commits with
  counts (`per_page=1` probes in parallel), sort select per type, results via
  `GET /api/v3/search/*` with `Accept: application/vnd.github.text-match+json`
  (abortable, page cache for back/forward), pagination (25/page, GitHub's
  1000-result window), `j/k/enter`, `shift+←/→` pages. Issues vs PRs add
  `is:issue` / `is:pr` unless the query has a type. Code results show the
  file header and the matching lines with line numbers (anchored on
  `line_numbers[0]`, ±1 line context) and highlighted spans; issue/repo/
  commit results highlight title/body/description/message matches.
* **Qualifier autocomplete** (`src/search/`): `qualifiers.ts` (definitions per
  type + `issue-list` / `pull-list` sets matching `pages/issues/filters.ts`,
  caret-aware `suggest` / `applySuggestion`, tested), `QueryInput.tsx`
  (combobox: ↑/↓, Tab or Enter-after-navigation accepts, Enter submits, Esc
  closes), `storeSource.ts` (values from the store: users + `@me`, labels,
  milestones, repos, owners, languages, topics; date/number templates).
  **Reused by the issue/PR list filter bar** (`IssueList.tsx`).
* `src/search/perf.ts`: latency samples, `window.__bghPerf.stats()`.

## Backend changes (additive)

* `bgh-search`: `/_bgh/search?org=` scope (repo keeps priority);
  `/_bgh/feed?org=` filter. Tests in `tests/palette.rs`, `tests/activity.rs`.
* `bgh-notify`: custom watching — migration `0510_watch_events.sql`
  (`watches.events TEXT[]`, NULL = all), `GET/PUT
  /_bgh/repos/{o}/{r}/subscription` (`{state: participating|all|ignore|custom,
  events: []}`), REST `PUT/DELETE /repos/{o}/{r}/subscription` reset
  `events`, fan-out delivers watcher notifications to custom watchers only
  for matching categories (Issue→issues, PullRequest→pulls,
  Release→releases). Tests in `tests/notifications.rs`.

## Shared web changes (small, additive)

* `app/Shell.tsx`: starts `startUnreadIndicators()`, lazy `WatchDialog`,
  commands "Watch settings…" and "Open search".
* `app/uiState.ts`: `watchRepoId`, `openWatch()`, `closeWatch()`.
* `app/TopBar.tsx`: "Search" breadcrumb. `app/routes.ts`: `/search`.
* `ui/icons.ts`: `BellSlashIcon`, `PulseIcon`, `RocketIcon`.
* `pages/issues/IssueList.tsx`: filter input → `QueryInput`.
* Mock backend: `src/mock/inboxSearch.ts` (done, thread/repo subscriptions,
  custom watching, `/_bgh/search`, `/search/*` with qualifiers + text
  matches, `/_bgh/feed` with cursor + `org=`), installed from `server.ts`.

## Verification

* `npm run typecheck && npm run lint && npm test && npm run build` green;
  unit tests: `search/qualifiers.test.ts`, `pages/notifications/inbox.test.ts`,
  `pages/dashboard/feed.test.ts`, `pages/search/highlight.test.ts`,
  `mock/inboxSearch.test.ts`.
* Real backend e2e: `web/scripts/inbox-e2e.sh [OUT_DIR]` (throwaway server
  + DB serving `web/dist`, users/tokens via `bgh admin`, REST seed
  `scripts/seed-inbox.mjs`, Playwright checks `scripts/inbox-search-e2e.mjs`).

### Performance (real backend, e2e run: 6 users, 256 issues/PRs, ~240 notifications, debug server build)

| metric | p50 | p95 | budget |
|---|---|---|---|
| palette local results (keystroke → committed render) | 1.6 ms | 2.8 ms | < 50 ms |
| palette local compute only | 0.3 ms | 0.5 ms | |
| palette server results (request start → rendered) | 11.4 ms | 15 ms | < 150 ms p50 |
| `/_bgh/search` fetch / server `took_ms` | 10 ms / 3.3 ms | 13 ms / 4.1 ms | |

(Measured with `window.__bghPerf.stats()` over 20 queries; written to
`OUT_DIR/perf.json` by the e2e script. Add the 60 ms debounce for
keystroke → server results.) Bundle: initial JS 125.9 KB gzip (budget 150);
new lazy chunks NotificationsPage ≈ 9 KB, SearchPage ≈ 6 KB, DashboardPage ≈
7.5 KB gzip.

The e2e checks: unread count in title + favicon badge; j/k + preview; e done
(optimistic + gone from `GET /notifications`); u read (server); s unsubscribe
(server); x bulk select + bulk done (server); URL filters, repo grouping,
saved custom view; watch dialog custom events (server); live arrival of a
mention via sync; palette perf + repo scope (Tab); code search highlights;
issue search pagination; qualifier autocomplete (key + value via Tab) on the
search page and the issue list filter; dashboard feed render, infinite
scroll, org context switch; no page errors.

## Known gaps / TODO

* Legacy `PUT /user/subscriptions/{o}/{r}` (bgh-repos `watching.rs`) upserts
  `watches` without resetting `events`, so re-watching through it keeps a
  custom selection (REST `/repos/{o}/{r}/subscription` does reset it).

* "Saved" (bookmarked) notifications: the sync model has no field for it.
* Custom views live in `localStorage` (per browser), not on the server.
* Thread subscription state isn't synced; it is fetched per previewed thread.
