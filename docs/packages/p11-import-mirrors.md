# P11 — Import repository from URL and pull mirrors — status

**Done.** Branch `bgh/p11-import-mirrors`, self-integrated into
`claude/sleepy-cray-9jj0t3`.

## Endpoints

All private (`/_bgh`), session or token auth. Credentials are write-only:
responses carry `has_credentials`, never the username or secret.

| Method + path | Who | What |
|---|---|---|
| `POST /_bgh/imports` | anyone who may create the repo (same rules as `POST /user/repos` / `/orgs/{org}/repos`, scopes incl.) | `{source_url, username?, password_or_token?, owner?, name?, description?, visibility?/private?, mirror, include_lfs, mirror_interval_minutes?}` → 201 import JSON. Creates the (empty) repository, the `repo_imports` row and, with `mirror`, the `repo_mirrors` row + `mirror_url`; enqueues `repos.import`. Userinfo in `source_url` becomes the credentials and is stripped. 422 `errors[].field = source_url` for non-http(s), invalid or SSRF-blocked URLs; name conflicts like repo creation. |
| `GET /_bgh/repos/{o}/{r}/import` | readers | status: `id, status (queued/importing/complete/failed/cancelled), phase, source_url, mirror, include_lfs, has_credentials, objects_received, objects_total, bytes_received, lfs_objects_received, lfs_objects_total, error, attempts, created_at, updated_at, completed_at, repository{id,name,full_name,owner,private,html_url,url}` |
| `POST …/import/cancel` | repo admins | queued/importing → cancelled (a running fetch is killed within ~1 s); else 422 |
| `POST …/import/retry` | repo admins | failed/cancelled → queued; optional `{username, password_or_token}` replaces the credentials (also of the mirror); else 422 |
| `GET /_bgh/repos/{o}/{r}/mirror` | repo admins | `url, interval_minutes, enabled, include_lfs, has_credentials, last_sync_at, next_sync_at, last_status (pending/success/failed), last_error, consecutive_failures, syncing` |
| `PATCH …/mirror` | repo admins | `url?, username?, password_or_token?, clear_credentials?, interval_minutes? (10–43200), enabled?, include_lfs?` |
| `DELETE …/mirror` | repo admins | convert to a regular repository (drops the mirror row, clears `mirror_url`) → 204 |
| `POST …/mirror/sync` | writers | enqueue `repos.mirror_sync` now → 202 |
| `GET /_bgh/admin/mirrors?status=failed\|all` | site admins | paginated mirrors (`repository, html_url` + mirror fields), failing ones by default |

REST `mirror_url` is set on every repository shape; GraphQL
`Repository.isMirror` / `mirrorUrl` are populated; repository search
`mirror:true|false` works; the sync `repo` model carries `mirrorUrl`.

## How it works

* **Fetch** (`bgh_git::fetch`): `git fetch --progress --force [--prune]
  +refs/heads/*:refs/heads/* +refs/tags/*:refs/tags/*` (hidden refs like
  `refs/pull/*` are never fetched) with a locked-down config passed via
  `GIT_CONFIG_COUNT`: only http/https protocols, no redirects, low-speed
  abort, no credential helpers/askpass, credentials as an
  `http.extraHeader` Authorization header (never argv/URL), and
  `http.curloptResolve` pinning the host to the addresses the SSRF check
  approved (no DNS rebinding). Progress is parsed from stderr
  (enumerate/count/receive/unpack/resolve, `Total N`). Ref updates are the
  diff of `for-each-ref` before/after. `remote_head` (`ls-remote
  --symref`) adopts the source's default branch. `lfs_pointers` scans
  `rev-list --objects` (incrementally for mirrors) for pointer blobs.
* **SSRF**: `bgh_core::ssrf` (moved from bgh-notify, which re-exports it)
  — same allow-list as webhooks (`BGH_WEBHOOK_ALLOWED_HOSTS` + the
  `webhooks.allowed_hosts` site setting). Checked syntactically on input
  and by resolution right before every fetch and every LFS request.
* **LFS**: batch API `download` against `<url>.git/info/lfs` (git-lfs'
  default endpoint), each href SSRF-checked and pinned, streamed into the
  shared LFS store with size + SHA-256 verification, then linked to the
  repository (`lfs_objects`, `lfs_size`).
* **Import job** `repos.import`: claims `queued → importing` and runs the
  import in a background task (imports can exceed the 10-minute job
  limit; fetch timeout 6 h). A writer task persists progress every second
  and keeps `updated_at` fresh; when the row stops being `importing`
  (cancel) it kills the fetch. The `repos.mirrors` service fails imports
  stale for > 2 min (dead process). Pushes are refused while an import is
  queued/running.
* **Mirrors**: `repos.mirrors` service (every 30 s, `FOR UPDATE SKIP
  LOCKED`, safe with several processes) enqueues `repos.mirror_sync` for
  due mirrors; the job fetches with `--prune`, LFS if enabled, records
  `last_*`/`consecutive_failures`, reschedules. Archived/disabled repos and
  repos with an unfinished import are skipped.
* **Post-receive**: fetched refs go through the same processing as pushes
  (`jobs::process_ref_updates`: pushed_at, size, default branch, languages,
  sync) and emit `Event::Push` with `origin: Some("mirror" | "import")`
  and the import/mirror creator as pusher: webhooks, search indexing and
  activity run; Actions ignores fetched pushes.
* **Read-only mirrors**: `RepoAccess::require_not_mirror()` (403 "This
  repository is a mirror and is read-only") on git push over HTTP and SSH,
  the refs/git-data/contents/branches (rename, merge, merge-upstream) APIs,
  LFS uploads and PR merges. `info/refs` 403s are now sent as
  `text/plain` so git prints the reason (`remote: …`).
* **Credentials**: sealed with `bgh_core::secretbox` (XChaCha20-Poly1305,
  the Actions server key: `BGH_ACTIONS_SECRET_KEY` or
  `{data_dir}/actions/server.key`). Never in API responses, audit entries
  (`repo.import`, `repo.import_cancel`, `repo.import_retry`,
  `repo.mirror_update`, `repo.mirror_convert` log URLs and flags only),
  logs or git argv.
* **Sync**: status changes are recorded as delta-only `repoImport` rows
  (`id, repoId, status, phase, error`) in `repo:{id}` (SYNC_PROTOCOL.md
  §3.2); the progress screen polls the REST endpoint every second.

## Web

* `/new/import` (`pages/new/ImportRepoPage.tsx`): clone URL (client
  validation, name suggested from the URL), optional credentials, owner /
  name / visibility, "Mirror the repository" (+ interval) and "Include Git
  LFS objects". Linked from `/new`, the top bar "+" menu and the command
  palette ("Import repository").
* `/:owner/:repo/import` (`pages/repo/ImportProgressPage.tsx`): phase
  steps with object/LFS counters, progress bar, success/failure banners,
  cancel, retry and retry with new credentials.
* Repo header: `Mirror` tag and "mirrored from <url>".
* Repo settings → **Mirror** (only for mirrors): status (last/next sync,
  last error), Sync now, URL, interval, pause, LFS, replace/remove
  credentials, Danger Zone "Convert repository".
* Site admin → **Mirrors** (`/site-admin/mirrors`, `g m`): failing / all
  mirrors with errors and Sync now.
* Mock backend: `src/mock/extra/imports.ts` (simulated ~3 s progress; URLs
  containing `fail` fail, retry succeeds).
* Playwright: `web/scripts/import-smoke.mjs` against a real server (see the
  header; verified: 24/24 checks incl. import, progress, mirror header,
  settings, admin list, convert).

## Tables / migrations

`migrations/2300_import_mirrors.sql`: `repositories.mirror_url`,
`repo_imports` (one row per repo, latest attempt), `repo_mirrors`
(indexes: due mirrors, failing mirrors, creators).

## Shared-code changes (all additive)

* `bgh-core`: `ssrf` (moved from bgh-notify; `bgh_notify::webhooks::ssrf`
  re-exports it), `secretbox` (copy of the Actions `ServerKey`, same key),
  `db::Repository::mirror_url` (+ `COLUMNS`), `api::MinimalRepository`
  fills `mirror_url`, `perms::RepoAccess::require_not_mirror` +
  `MIRROR_READ_ONLY`, `events::PushEvent::origin` (`#[serde(default)]`,
  `ORIGIN_MIRROR`/`ORIGIN_IMPORT`, `is_fetched()`; existing literals got
  `origin: None`), sync `repo` shape `mirrorUrl`. Deps: `url`,
  `chacha20poly1305`.
* `bgh-git`: new `fetch` module.
* `bgh-repos`: `import.rs`, `mirrors.rs`; `create.rs` split into
  `create_with` / `authorize_org`; `jobs::process_ref_updates`;
  `git_http` plain-text 403s. Deps: `reqwest`, `url`.
* `bgh-actions/src/trigger.rs`: one guard arm (fetched pushes don't
  trigger). `bgh-pulls/src/merge.rs`: `require_not_mirror` on merge.
  `bgh-graphql` `isMirror`/`mirrorUrl`; `bgh-search` `mirror:` qualifier.

## Tests

`crates/bgh-repos/tests/it/import.rs` (the source is a repository served
by the test server itself, loopback allow-listed):
`imports_branches_tags_and_lfs` (private source + credentials, branches,
annotated tag, LFS object downloaded and linked, refs equal, clone + push
work, no credentials in responses/audit/storage, `repoImport` deltas),
`mirror_syncs_and_is_read_only` (`mirror_url` in REST/GraphQL/search, push
and ref API rejected, upstream commit + branch deletion arrive with Sync
now, Push event has origin `mirror`, settings validation, failure
reporting, admin list, convert), `scheduler_enqueues_due_mirrors`,
`import_validation_ssrf_and_retry` (loopback/file/ssh/metadata URLs → 422,
wrong credentials fail, retry with new credentials succeeds),
`cancel_and_stale_imports` (cancel before run, push refused while
importing, stale sweep). Unit tests in `bgh_git::fetch`,
`bgh_core::secretbox`, `bgh_repos::import`.

## Known gaps / follow-ups

* Only `http(s)` sources (no `ssh://`/`git://`), basic auth only.
* No GitHub "Source Imports" REST API (`/repos/{o}/{r}/import`; deprecated
  upstream) and no wiki import — P18/P51 build metadata import on top of
  `POST /_bgh/imports` (without `mirror`).
* Mirror syncs run inside the 10-minute job limit (fetch timeout 9 min);
  very large first fetches should go through an import (6 h).
* Progress counters are polled, not pushed (only status changes sync).
* Behind an HTTP proxy (`https_proxy`), git/reqwest connect through the
  proxy and the address pinning has no effect; the allow-list check on the
  URL still applies.
