# P12 repo-nav — status

**In progress → integrating.** Branch `bgh/p12-repo-nav`. Scope:
`docs/PHASE4_PLAN.md` §P12 (no §5 quick fixes are assigned to P12), plus a
check that the compare page (`/{o}/{r}/compare/{base}...{head}`) works.
No migrations (2400–2499 unused).

## What changed

### Web (`web/src/pages/repo`)

* **Header** (`RepoLayout.tsx`):
  * **Fork** opens `ForkDialog.tsx` (lazy): owner (viewer + their orgs,
    minus the repo's owner; the server enforces org create rights and its
    422/403 message is shown), name (normalized like `/new`), description,
    "Copy the `<default>` branch only" (default on). Submits
    `POST /repos/{o}/{r}/forks`, shows "Forking…" while the request runs,
    then navigates to the fork. Disabled when `allow_forking` is false on a
    private repo. Signed-out clicks go to `/login?return_to=`.
  * **Watch** opens the existing `WatchDialog` (`ui.openWatch`). Label:
    Watch (participating) / Unwatch (all activity) / Custom (custom events,
    read from `/_bgh/repos/{o}/{r}/subscription`, cache key
    `watchSettingsKey`, updated optimistically by `saveWatchSettings`) /
    Ignoring.
  * Star / watch / fork **counts are links** to `/:o/:r/stargazers`,
    `/watchers` and `/forks`.
  * "forked from X" and "generated from X" line, template / fork repo
    icons, "Public template" tag, from the REST repository (`parent`,
    `template_repository`, `is_template`; resource key `restRepoKey`, 60 s).
  * **Sync fork** (`SyncFork.tsx`): popover with ahead/behind of the
    fork's default branch vs `upstream:branch` (`GET compare` on the fork),
    "Compare" and "Update branch" (`POST /merge-upstream`, push access
    only). A 409 shows the conflict message and an "Open pull request"
    link to the compare page.
  * **Use this template** on template repos → `/new?template_owner=&template_name=`.
  * Issues / Projects tabs hidden when `has_issues` / `has_projects` is
    false (`visibleRepoTabs` in `nav.ts`).
  * **Rename / transfer redirect**: when the URL's repo is not in the store
    and `GET /repos/{o}/{r}` resolves (server `repo_redirects`) to another
    `full_name`, `navigate(..., { replace: true })` to the canonical
    owner/name keeping sub-path, query and hash (`canonicalRepoUrl`).
    Fixes the endless spinner.
* **New pages / routes** (`web/src/app/routes.ts`, additive):
  * `/:o/:r/stargazers`, `/:o/:r/watchers` (`RepoPeoplePage.tsx`, reuses
    the profile `UserList`), `/:o/:r/forks` (`ForksPage.tsx`, sort
    newest/oldest/stargazers/watchers) — paginated via `usePagedList`
    (`Link: rel="next"`, "Load more").
  * `/:o/:r/runs/:id` (`CheckRunPage.tsx`): same-origin `details_url`
    (Actions jobs) → replace to that page; otherwise a summary card with a
    link to the external `details_url` (http(s) only).
  * `/:o/:r/labels/:name` → issues filtered by the label; `/:o/:r/search`
    → `/search?q=repo:o/r …&type=code`; `/orgs/:org/people`,
    `/orgs/:org/repositories`, `/orgs/:org/teams` → org profile tabs
    (`AliasPage.tsx` + pure `redirects.ts`).
  * `/orgs/:org/teams/:team` → the org-settings team page (`OrgTeamPage`),
    which already limits editing to owners and team maintainers.
* **Real 404s**: `RepoPlaceholderPage` now renders `NotFound` for every
  unknown repo sub-path; only `security` and `pulse` keep the "coming
  soon" placeholder (until P66 / P31).
* **Compare page**: verified working end to end against the real backend
  (cross-fork `main...bob:feature`: commits, files, diff, "Create pull
  request" form). No changes needed.
* API wrappers (`api/endpoints.ts`): `createFork`, `mergeUpstream`,
  `getCheckRun`, `repoListPaths`; `RestRepository` gained optional
  `parent` / `source` / `template_repository` / `is_template` / `has_*` /
  `permissions`; new `RestRepoRef`, `RestCheckRun`, `MergeUpstreamResult`.
* **Mock** (`src/mock/extra/repoNav.ts`): fork create/list, `parent` on the
  full repository, stargazers/subscribers with `Link` pagination,
  cross-fork compare + merge-upstream for Sync fork, single check runs.

### Backend (`crates/bgh-repos`)

* `POST /repos/{o}/{r}/forks` accepts an optional `description`
  (github.com's fork form has it; the REST API ignores unknown fields, so
  clients are unaffected). Test: `forks::fork_list_and_existing_fork`.

## Tests

* Vitest: `src/pages/repo/nav.test.ts` (redirect logic incl. sub-path /
  query / hash, tab visibility, watch label, sync summary, alias targets,
  check-run `details_url`), `src/mock/extra/repoNav.test.ts`.
* Smoke (real backend, Playwright): `web/scripts/smoke-repo-nav.mjs` —
  fork from the UI, watch dialog from the header, Sync fork of a fork
  that is behind, stargazers/forks/watchers pages, cross-fork compare,
  "Use this template" + "generated from", `/o/r/doesnotexist` 404, Insights
  placeholder, label/org/team/check-run `html_url`s, rename redirect.
  All 23 checks pass.

## Shared-code changes

* `web/src/pages/notifications/actions.ts`: exported `watchSettingsKey`;
  `saveWatchSettings` updates that resource-cache entry (additive).
* `web/src/app/routes.ts`: additive entries above the `/:owner/:repo/:tab`
  catch-all.

## Known gaps

* The fork dialog lists all of the viewer's organizations; whether a
  member may create repositories there is checked by the server (403
  message shown in the dialog), not pre-filtered.
* Forking is synchronous on this server, so "Forking…" covers the request
  only (no polling for a background 202).
* Team page back-link still points at org settings → teams.
