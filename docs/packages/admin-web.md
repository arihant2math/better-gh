# F7 admin-web — status

Branch `bgh/admin-web` (merged with the integration branch, which now
contains admin, accounts, notify and sync). Web package; no new
crates or migrations. **Status: complete** (gaps below need backend work).

Verified with `npm run typecheck && npm run lint && npm test && npm run
build` (bundle budget OK: initial JS 123.2 KB gzip, about +3 KB over the base
for the route table, `app/site.ts` and the admin palette commands; every
admin/org page is its own lazy chunk, largest 28 KB raw) and with Playwright
against a real backend (`web/scripts/admin-smoke.mjs`: 22 checks, all
passing against the integrated server; screenshots in light and dark mode).

## What's built

**App-wide** (initial bundle, tiny)
* `app/site.ts`: loads `GET /_bgh/site` after sign-in (refresh every 5 min)
  and whether the viewer is a site admin (`BootUser.siteAdmin` when the
  server sends it, else one `GET /api/v3/user`).
* Announcement banner (expiry respected, per-message dismissal in
  localStorage when `user_dismissible`) and maintenance banner (on, or
  scheduled), above the top bar; the banner component is a lazy chunk
  loaded only while one is active.
* "Site admin" in the user menu + `Site admin: …` palette commands for
  admins; "Settings" link on org profiles for owners / site admins;
  breadcrumbs for `/site-admin` and org settings.

**Site admin** (`/site-admin/*`, `pages/admin`, layout guards non-admins;
`g d/u/o/r/e/a/j/w` jump between sections)
* Dashboard: overall status, version, uptime; KPI tiles (users, orgs,
  repos, pushes, issues, PRs, hooks); component health (DB, Redis, job
  queue, git, storage) with live latency / queue-depth sparklines sampled
  client-side every 10 s; disk meter + used-space split; queue-by-kind bars;
  activity breakdown stacked bars. Hand-rolled SVG, palette validated for
  CVD/contrast in both themes (`--chart-*` tokens).
* Users: search/filter (admins, suspended, dormant, no 2FA)/server sort,
  virtualized infinite table, keyboard; create user. Detail: profile,
  emails, SSH/GPG keys, orgs, repos, 2FA, sessions, tokens, storage quota
  (meter + edit/reset); actions: suspend (reason)/unsuspend,
  promote/demote, rename, password reset (generate or set; shown once),
  disable 2FA, sign out everywhere, impersonation token (scopes; shown once)
  and revoke all, delete with repository transfer.
* Organizations: list/search/sort, create; detail (members, teams, repos,
  quota, settings), rename, archive/unarchive, delete with transfer, link to
  org settings.
* Repositories: list with visibility/archived/disabled/fork filters and
  sorts, maintenance on all repos; detail with counts, sizes, storage path,
  visibility change, rename, transfer, archive, disable, delete,
  maintenance (gc/repack/fsck/recalculate size/languages) with live run
  list and output.
* Site settings: sign-up policy + allowed domains, default visibility, max
  repo size, org creation, announcement (expiry, dismissible, live
  preview), rate limits, password login + OIDC providers (secrets
  write-only), SMTP, maintenance mode (confirm + preview). Per-section
  dirty tracking, sticky save bar (⌘S), client validation, 422 mapped to
  sections, `beforeunload` guard, nav dot while dirty.
* Audit log: phrase search + actor/action/repo/org/user/date filters in
  the URL, newest/oldest, cursor infinite scroll, detail drawer with
  "filter by" shortcuts, CSV export (loaded rows or all matching ≤ 10 000).
* Background jobs: totals + per-kind table, retry failed (all / per kind),
  state/kind filters, job drawer (payload, error, retry/cancel), 5 s
  auto-refresh (pausable).
* Global webhooks: list, create/edit (events allowed by the backend,
  write-only secret, SSL toggle), activate/deactivate, ping, delete.

**Organization settings** (`/organizations/:org/settings/*`,
`pages/orgsettings`; GitHub REST endpoints from B1 accounts, B5 notify and
`/orgs/{org}/audit-log` from B7)
* General (profile, avatar upload/remove, base permission and member
  privileges, save bar), Members (roles, remove, convert to outside
  collaborator, invite by login/email with teams), Teams (nested tree,
  create), Team (members + roles, child teams, repositories + permission,
  settings, delete), Outside collaborators, Invitations (pending/failed,
  cancel), Audit log (phrase, cursor, drawer, CSV), Webhooks (CRUD, ping,
  deliveries with request/response drawer and redeliver).

## Shared-code changes (all additive)

* `web/src/api/cache.ts`: `refresh(key, loader)`, `mutate(key, fn)`.
* `web/src/components/admin/*` (new kit), `web/src/app/site.ts`,
  `SiteBanners.tsx` (new); `Shell.tsx`, `Sidebar.tsx`, `TopBar.tsx`,
  `routes.ts`, `boot.ts` (`BootUser.siteAdmin?`), `ProfilePage` (settings
  link), `ui/icons.ts` (new icons), `ui/tokens.css` (`--chart-*`).
* `web/vite.config.ts`: dev proxy for `/avatars`.
* `bgh-core::settings::maintenance_exempt`: the web sign-in endpoints
  (`/_bgh/auth/login`, `/_bgh/auth/2fa`, `/_bgh/auth/logout`, `/_bgh/boot`,
  `/_bgh/session/two_factor`) pass the maintenance gate — before, site
  admins could not sign in through the web client to turn maintenance off
  (test in `bgh-admin/tests/settings.rs`).
* Docs: `docs/FRONTEND.md` "Admin-style pages", `web/README.md`.

## Known gaps / TODO

* "Send test email" is disabled: no backend endpoint.
* Global webhook deliveries are not exposed by `/admin/hooks` (org hooks
  have them).
* Boot data has no `siteAdmin`; the client makes one `GET /api/v3/user`
  (B1 could add `siteAdmin` to `BootUser`).
* `GET /orgs/{org}` doesn't return `members_can_create_teams` /
  `web_commit_signoff_required` (accepted on PATCH), so the General page
  can't show their current values; the org list of the admin API has no
  archived flag; team lists have no member counts (one request per visible
  row).
* No mock-backend support for admin/org-settings pages (use a real server).
