Integration: pending (gate running)
GitHub/GHES metadata importer (new crate `bgh-import`, migration 3000), CLI, site-admin and org Import UI; full gate green on the merged branch.

# P18 — Metadata importer, part 1: GitHub/GHES issues, labels, milestones, releases, users — status

**Done.** Branch `bgh/p18-metadata-import`, merged with the integration
branch and gate-green; waiting for the integrator (WORKER_GUIDE rule 14).
New crate `bgh-import`; migration `3000_metadata_import.sql`.

## What it does

Imports one repository from GitHub.com or GitHub Enterprise Server
(`https://api.github.com` or `https://HOST/api/v3`, optional token) into a
new repository on this server, in steps:

| Step | What |
|---|---|
| `git` | P11 import (`bgh_repos::import`, no mirror) of `clone_url`, token as `x-access-token` basic auth, LFS optional. The run waits for it. |
| `settings` | description, homepage, topics (valid ones, ≤ 20), `has_issues/projects/wiki/discussions` |
| `labels` | name, color, description, default. The new repository's seeded default labels are dropped when the import is created (only then: nothing references them yet), so the target mirrors the source's set. |
| `milestones` | **original numbers**, state, due date, creator, timestamps; `open_issues`/`closed_issues` recomputed at the end |
| `issues` | **original numbers**, title, body, state + `state_reason`, author, assignees, labels, milestone, lock state + reason, `closed_at`/`closed_by`, `created_at`/`updated_at`, reactions (users mapped). Pull requests are skipped (P51) but count towards the number sequence. |
| `comments` | issue comments (`/issues/comments`, repo-wide) with authors, timestamps and reactions; comments on PRs are left for P51 |
| `events` | `closed`, `reopened`, `labeled`, `unlabeled`, `milestoned`, `demilestoned`, `assigned`, `unassigned`, `locked`, `unlocked`, `renamed` (repo-wide `/issues/events`), original actor and time, stored in the client data shape (BACKEND_PATTERNS §8a). Others (`subscribed`, `mentioned`, `referenced`, …) are dropped. |
| `releases` | tag, target, name, body, draft/prerelease, author, `created_at`/`published_at`; assets downloaded (`Accept: application/octet-stream`, redirect to the storage host followed) with name, label, content type, download count, uploader. A release whose tag is missing and can't be created is imported as a draft (logged). |
| `teams` | optional, organization targets: teams with access to the source repository (found or created by slug) get the mapped permission on the target; their members that are already members of the target organization join (never mannequins). |
| `finish` | `next_issue_number` = max(source issue **or PR** number) + 1, so P51 can insert PRs with their numbers; milestone counters; repo re-sync; code search reindex (`search.index_repo`) |

### Users

Per source user (cached per run, mapping shared by every import from the
same source host — `import_mappings` scope = host):

1. an existing mapping to a real account,
2. the source profile's public email (`GET /users/{login}`) matching a
   **verified** local email,
3. the import's login map (`source → local`),
4. else a **mannequin**: a `users` row with `mannequin = true`,
   `mannequin_source` (host), `mannequin_login` (source login), login
   `{source}-imported` (`-2`, … on collisions; never the bare source login,
   which a real person may want), display name = the source login, source
   avatar URL, **no password and no email** — so no session, token, password
   login or reset is possible. An existing mannequin mapping is only a
   fallback: a later email/login-map match replaces it for that and later
   imports (moving what earlier imports attributed to the mannequin is P51's
   reclaim).

`ghost` / `null` users stay ghost.

### Import mode (no side effects)

The internal insert APIs (`bgh_issues::import`, `bgh_releases::import`) keep
source numbers, authors and timestamps and **emit no domain events**: no
webhooks, notifications, activity, Actions, mentions, cross-references or
subscriptions. They still record sync actions (issues with body, comments,
events, labels, milestones, the repo row, release rows), so open clients see
the import live. The git step's push carries `PushEvent.origin =
"metadata-import"` (`PushEvent::is_quiet`, `Event::is_quiet`): webhooks,
notifications, activity and commit-keyword closing skip it, Actions skips it
as a fetched push, code search still indexes it. The repository's creation is
a regular `repository.created` (it is news).

### Resumable, idempotent, rate-limit aware

* Every source object is written **in one transaction with its
  `import_mappings` row** (`import:{id}` scope) and its `imports.stats`
  counter, so counts follow the mappings and a rerun skips what exists.
  Paged steps keep the page being processed in `imports.cursor`; a resume
  replays that page.
* `import.run` job (3 attempts) claims `queued`/`waiting` → `running` and
  runs in a background task (like P11; imports outlive the 10-minute job
  limit). The run heartbeats every 2 s and stops when the row leaves
  `running` (cancel). The `import.sweeper` service (every 60 s, `bgh serve`)
  re-queues runs whose heartbeat is older than 2 min (dead process), up to 5
  times, then fails them.
* Conditional requests: each page's `ETag` + body in `import_http_cache`; a
  rerun/resume sends `If-None-Match` and reuses the page on `304` (free on
  GitHub's rate limit).
* Primary (`X-RateLimit-Remaining: 0` → `X-RateLimit-Reset`) and secondary
  (`Retry-After`, 403/429) limits: waits ≤ 60 s sleep in-process; longer
  ones park the run as `waiting` with `resume_at` and a job scheduled then.
  5xx and connection errors retry 4× with backoff (1, 2, 4, 8 s).
* Every host is SSRF-checked and pinned (`bgh_core::ssrf`, the webhook
  allow-list; redirects followed by hand and re-checked). The token goes
  only to the API origin (dropped on the asset redirect), is sealed with
  `bgh_core::secretbox`, and never appears in responses, logs or the audit
  log.

## Endpoints (private, session or token auth)

Who: site administrators (any owner) and organization owners (their
organization). Personal-account targets are site-admin only (mannequins are
site-wide accounts). Rows are visible to site admins, their creator and the
target organization's owners; others get 404.

| Method + path | What |
|---|---|
| `POST /_bgh/metadata-imports` | `{api_url?, source_repo, token?, owner, name?, visibility?, git?, settings?, labels?, milestones?, issues?, releases?, teams?, include_lfs?, user_map?}` → 201 import JSON. Checks the source synchronously (`GET /repos/{source}`): 422 `errors[].field` = `source_repo` (format, not found / not readable), `token` (bad credentials), `api_url` (scheme, SSRF, unreachable), `owner`; repo name conflicts like repo creation; 403 for non-owners. Creates the repository (+ P11 import) and queues the run. |
| `GET /_bgh/metadata-imports/{id}` | `id, kind, api_url, source_repo, source_url, has_token, owner, repo_name, visibility, repository{id,name,full_name,private,html_url,url}, options{…, user_map_entries}, status (queued/running/waiting/complete/failed/cancelled), step, steps[{name, state: pending/running/done/failed/skipped}], stats{issues, comments, events, reactions, labels, milestones, releases, assets, teams, users_mapped, mannequins, max_number}, git{status, phase, objects_received, objects_total, error}, error, attempts, resume_at, created_at, updated_at, completed_at` |
| `GET …/{id}/log?after=N` | `{entries: [{id, level (info/warn/error), message, created_at}]}`, oldest first, ≤ 500 |
| `POST …/{id}/cancel` | queued/running/waiting → cancelled (also cancels the P11 git import in flight); else 422 |
| `POST …/{id}/resume` | `{token?}` failed/cancelled/stale-running → queued (continues at its step; a failed/cancelled git step is re-queued too); complete → rerun from the start (only new source objects); else 422 |
| `GET /_bgh/admin/metadata-imports` | site admins, paginated (`Link`), newest first |
| `GET /_bgh/orgs/{org}/metadata-imports` | organization owners (members 403, others 404), paginated |

Audit: `repo.metadata_import`, `repo.metadata_import_cancel`,
`repo.metadata_import_resume` (URLs, ids and flags only).

## CLI

```
BGH_IMPORT_TOKEN=ghp_… bgh import github --repo octo-org/hello-world --owner acme \
    [--api-url https://ghe.example/api/v3] [--name N] [--visibility private] \
    [--user-map map.csv] [--skip releases,…] [--teams] [--include-lfs] [--as admin] [--detach]
bgh import resume --id N [--detach]
```

Runs as a site admin (`--as`, default the first). Without `--detach` it runs
job workers in-process and prints the log until the run ends (exit status 1
on failure). The login map file: `source,local` (or `=`/whitespace) per
line, `#` comments; GitHub Enterprise Importer's mannequin CSV
(`mannequin-user,mannequin-id,target-user`) works as is.

## Web

* **Site admin → Imports** (`/site-admin/imports`, `g p`): imports list,
  "New import" form, detail `/site-admin/imports/:id`.
* **Organization settings → Import** (`/organizations/:org/settings/import`,
  `g p`, owners): the form with the owner fixed, previous imports, detail
  `…/import/:id`.
* Form (`pages/imports/ImportForm.tsx`): GitHub.com / GHES host, source
  (`owner/name` or URL), token (write-only), owner/name/visibility (default:
  the source's), step checkboxes, login map textarea (client-validated);
  server field errors shown per field.
* Detail (`pages/imports/ImportDetail.tsx`): status, source → target links,
  steps with state icons (git phase/percent while fetching), counters, the
  log (incremental polling every 1.5 s while active), cancel, resume, resume
  with a new token, "Import again".
* Mock: `src/mock/extra/metadataImports.ts` (simulated ~4 s run; sources
  containing `fail` fail at comments until resumed; token `bad` → 422).
* Playwright: `web/scripts/metadata-import-smoke.mjs` against a real server,
  using the server's own API as a GHES source (see its header). Verified
  20/20 checks: new import with a login map, live completion, steps,
  counters, log, imported author/timestamps/state reason/asset, "Import
  again" adds nothing, source validation message, org settings page with
  the fixed owner, org detail, no page errors. `bgh import github` was run
  against the same server (3 issues, comment, reaction, 7 events, release +
  asset).

## Tables / migrations (`3000_metadata_import.sql`)

* `users.mannequin`, `mannequin_source`, `mannequin_login` (+ partial index).
* `imports` (one row per import: source, sealed token, owner, target, options,
  status, step, cursor, stats, error, attempts, `resume_at`, `heartbeat_at`;
  indexes on owner, repo, creator, active rows).
* `import_mappings (scope, source_type, source_id) → local_id` (+ import
  index).
* `import_log`, `import_http_cache (import_id, url) → etag, body, link`.
* `repo_imports.quiet` (P11's table): the git step's push is
  `metadata-import`.

## Shared-code changes (additive)

* `bgh-core/events.rs`: `PushEvent::ORIGIN_METADATA_IMPORT`,
  `PushEvent::is_quiet`, `Event::is_quiet`.
* Listener guards (one `if event.is_quiet()` each): `notify.webhooks`
  (`webhooks/dispatch.rs`), `notify.notifications` (`fanout.rs`),
  `search.activity` (`activity/record.rs`), `issues.commit_references`
  (`refs.rs`).
* `bgh-issues`: new `pub mod import` (`upsert_label`, `insert_milestone`,
  `insert_issue`, `insert_comment`, `insert_event`, `insert_reactions`,
  `finish_repo`).
* `bgh-releases`: new `pub mod import` (`ensure_tag`, `insert_release`,
  `store_blob`, `insert_asset`).
* `bgh-repos`: `create::create_with` is `pub`; `NewImport.quiet`,
  `ImportRow.quiet` (origin choice in `run_import`).
* `bgh-server`: crate wired in `register`/`app`; `bgh import github|resume`.
* Workspace: `bgh-import` crate.

## Tests

`cargo test -p bgh-import` (one `it` binary + unit tests):

* `github::imports_issues_labels_milestones_releases_and_users` — the
  acceptance test: an in-test fake GitHub API (`tests/it/fake.rs`, axum
  serving `fixtures/github/` with token auth, two-page `Link` pagination
  through `/repositories/{id}/…`, `ETag`/304, the asset redirect to another
  origin, a primary and a secondary rate limit) imports issues 1, 2, 5
  (3, 4 are PRs) with comments, reactions, events, labels, a milestone, a
  release with an asset and a team. Numbers, authors (verified email, login
  map, mannequin) and timestamps are preserved; next issue is #6; no
  webhooks (an org hook to `*` receives only `repository`), activity or
  notifications; no token in responses, logs or the audit log; the token is
  not sent to the storage host. A rerun is a no-op (same rows and stats,
  answered with 304s).
* `github::resumes_after_a_killed_run_and_after_a_failure` — a run killed
  after 6 objects (`pipeline::run(…, Some(6))` returns leaving the row
  `running`, exactly as a dead process would) is refused a manual resume
  while live, re-queued by the sweeper once stale, fails at the comments
  step on a source error (410), and finishes after resume with nothing
  duplicated.
* `github::a_later_match_replaces_a_mannequin_mapping`.
* `github::validation_and_permissions` — 422 fields, SSRF, bad token,
  missing source, permissions (org member 403, org owner 201, personal
  account), name conflicts, visibility (404), admin/org lists with `Link`
  paging, cancel (git step included), cancelled → resume → complete.
* `self_import::imports_from_this_servers_own_api` — the importer against
  this server's own GitHub-compatible REST API (labels, milestone, issues,
  comment, reaction, `not_planned` close, release + asset, topics, tags).
* `recorded::*` — real api.github.com responses (`fixtures/github-recorded/`,
  repository + labels; see its README for what could be recorded here) carry
  every field the importer reads.
* Unit tests: `Link` parsing, hosts/URLs/options, mannequin logins, source
  repo validation, login-map files.

The `fixtures/github/` responses are hand-written to GitHub's documented
shapes, not captured from live traffic (this environment can only read its
own repository on GitHub; issues/events/releases/users couldn't be
recorded).

## Known gaps / follow-ups

* Pull requests, reviews, review comments, PR conversation comments and PR
  events, wikis, GitLab, mannequin reclaim: **P51** (PR numbers are already
  reserved: the sequence continues after the highest source number).
* Not imported: reactions on releases, sub-issues, issue types, issue
  dependencies, projects, discussions, commit comments, branch protection,
  webhooks, `pinned` state, issue/comment edit history, `referenced` /
  `cross-referenced` / `connected` events.
* Email matching needs the source user's **public** email (GitHub hides
  private ones even with a token); most users therefore need the login map.
* `/issues/events` on GitHub may not reach very old events of huge
  repositories; per-issue `/timeline` fetching would fill that in (more
  requests).
* Team members who aren't members of the target organization are only
  logged, not invited.
* Bodies keep source-relative links/mentions as written (`#123` stays
  correct because numbers are preserved; `@login` mentions of mannequins
  point at the source login).
* Progress isn't pushed over sync (the UI polls the REST status and log).
