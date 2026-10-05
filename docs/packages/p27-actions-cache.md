Integration: landed
Actions cache (native `actions/cache`, legacy + twirp cache protocols, Azure Blob subset), twirp ArtifactService, runtime token/env, `/actions/caches` REST, caches web page.

# P27 — Actions cache and toolkit runtime services: status

Branch `bgh/p27-actions-cache`. Scope: `docs/PHASE4_PLAN.md` §P27 (no §5
quick fixes are assigned to P27). Migration range 3900–3999 (used:
`3900_actions_cache.sql`).

## Runtime environment

Every job gets (minted when a runner claims it, `JobSpec.runtime_token`,
serde default so older runners/specs still work; masked in logs):

| Variable | Value |
|---|---|
| `ACTIONS_RUNTIME_TOKEN` | HS256 JWT (key derived from the Actions server key), claims `job`/`run`/`repo`, `exp` = job timeout + 60 min, `scp` = `Actions.Results:<run>:<job> …` (what `@actions/artifact` parses for its backend ids). Accepted only while the job is `in_progress`. |
| `ACTIONS_RUNTIME_URL`, `ACTIONS_CACHE_URL` | `<server>/_bgh/actions/runtime/` |
| `ACTIONS_RESULTS_URL` | `<server>/` (twirp at `/twirp/...`) |

Module `bgh_actions::runtime` (token mint/verify, `RuntimeJob` extractor,
read/write cache scopes, signed blob URLs, `job_env`).

## Cache (`bgh_actions::cache`)

* Store: `actions_caches` rows + `{data_dir}/actions/caches/{id}` archives
  (`{id}.part` / `{id}.blocks/` while uploading).
* Scoping like GitHub: entries belong to the run's ref; a restore searches
  the run's ref, then the PR base branch, then the default branch; in each
  scope exact primary key, then newest prefix match of the primary key,
  then of each restore key; the version must match. Entries are immutable
  (unique `(repo, ref, key, version)`); a reservation left by a finished
  job is taken over.
* Size limit per repository: `BGH_ACTIONS_CACHE_SIZE_LIMIT_GB` (default 10,
  `ActionsConfig.cache_size_limit` in bytes), optionally lowered per repo
  (`actions_cache_policies`). LRU eviction after every commit (one window
  query). Expiry: `BGH_ACTIONS_CACHE_RETENTION_DAYS` (default 7) since last
  access; uncommitted reservations after 24 h; run by the existing
  `actions.maintenance` loop.
* Legacy protocol (`cache::v1`, `@actions/cache` on non-github.com hosts and
  the native runner): `GET _apis/artifactcache/cache?keys=&version=` (200 /
  204), `POST caches` (201 `{cacheId}`, 409 exists, 400 too large),
  `PATCH caches/{id}` with `Content-Range` (parallel/out-of-order chunks),
  `POST caches/{id}` `{size}` commit. Errors in Azure DevOps shape
  (`typeKey`, `message`).
* Twirp `github.actions.results.api.v1.CacheService` (`cache::twirp`):
  `CreateCacheEntry`, `FinalizeCacheEntryUpload`, `GetCacheEntryDownloadURL`
  (misses / duplicates are `{"ok": false}` like GitHub).

## Results service (`bgh_actions::results`)

* Twirp plumbing: `POST /twirp/{service}/{method}`, JSON only, protobuf
  JSON decoding (snake_case and lowerCamelCase, int64 as strings,
  wrapper types), twirp error codes/statuses.
* `ArtifactService`: `CreateArtifact`, `FinalizeArtifact`, `ListArtifacts`
  (`name_filter`, `id_filter`), `GetSignedArtifactURL`, `DeleteArtifact`.
  Backend ids must be the token's run/job. Finalized uploads are stored with
  the existing `server::store_artifact`, so REST artifacts / zip download /
  the run page show them.
* Azure Blob subset (`results::blob`) at
  `/_bgh/actions/blob/{cache|artifact-upload|artifact}/{id}?se=&sp=&sig=`
  (HMAC-signed, expiring, read or write): Put Blob (`x-ms-blob-type`
  required), Put Block (`comp=block&blockid=`), Put Block List
  (`comp=blocklist`, `Latest`/`Uncommitted`/`Committed`), Get Blob with
  `Range` / `x-ms-range` (206, 416 `InvalidRange`), HEAD properties;
  `x-ms-request-id`, `x-ms-version`, `ETag`, `Last-Modified`; XML errors with
  `x-ms-error-code`. Bodies are streamed to disk.

## Native actions/cache (`runner/cache.rs`)

`actions/cache` (restore, then a `Post` step saving unless the primary key
hit; `save-always` → `always()`), `actions/cache/restore` (outputs
`cache-hit`, `cache-primary-key`, `cache-matched-key`; `fail-on-cache-miss`,
`lookup-only`) and `actions/cache/save`. Archives are made/extracted with
`tar -z -P` inside the job environment (so `~` is the job container's home;
globs incl. `**` under bash, `!` excludes), in `@actions/cache`'s layout and
version hash for gzip, and moved through the legacy protocol over HTTP with
the runtime token — the same path for the built-in and external runners.
Service failures are warnings, as in the real action. `cache-hit` is empty
on a miss (v4 behaviour).

## REST (`api/caches.rs`)

* `GET /repos/{o}/{r}/actions/caches` (`key` prefix, `ref` incl. bare
  branch names, `sort` created_at|last_accessed_at|size_in_bytes,
  `direction`, pagination + `Link`), `DELETE …/actions/caches?key=&ref=`
  (200 with the deleted list, 404 none, 422 without key),
  `DELETE …/actions/caches/{id}` (204).
* `GET /repos/{o}/{r}/actions/cache/usage`, `GET|PATCH
  …/actions/cache/usage-policy` (GHES; admin, 1..site limit GB).
* `GET /orgs/{org}/actions/cache/usage`, `…/usage-by-repository` (org
  owners, `read:org`).
* Permissions: read access to list (job tokens: `actions: read`), write to
  delete (`actions: write`), via the existing `actions/**` route class.

## Web

`/:owner/:repo/actions/caches` (lazy chunk `pages/actions/caches/CachesPage`,
~3 KB gzip; initial JS unchanged at 143.3 KB): usage meter against the
repository limit, key-prefix search, branch filter, sort menu, load more,
delete with confirmation. "Caches" link in the Actions sidebar
(Management). API client `web/src/api/caches.ts`; mock
`web/src/mock/caches.ts` (+ `caches.test.ts`). Verified with Playwright in
mock mode (list, filter by key and branch, sort, delete, no console errors).

## Tables / migrations

`3900_actions_cache.sql`: `actions_caches` (unique entry key, partial
lookup index with `text_pattern_ops` for prefix matches, LRU/expiry
indexes), `actions_cache_policies`.

## Shared-code changes (additive)

* `bgh_core::config::ActionsConfig`: `cache_size_limit`
  (`BGH_ACTIONS_CACHE_SIZE_LIMIT_GB`), `cache_retention_days`
  (`BGH_ACTIONS_CACHE_RETENTION_DAYS`).
* bgh-actions (P29/P16/P26 share this crate): new modules `runtime`,
  `cache`, `results`, `api::caches`, `runner::cache`; `JobSpec.runtime_token`
  (serde default), `PostKind::CacheSave`, one call in `JobRunner::base_env`,
  `ServerKey::derive`, a line in `services::maintenance`, routes in `lib.rs`.
  Workspace dep `hmac` added to bgh-actions.
* `scripts/gh-compat.sh`: `gh cache list`, `gh cache list --json`,
  `gh cache delete <key>`, `gh cache delete --all`, cache usage (entries are
  seeded with SQL in the throwaway database since that server runs no jobs).
* Docs: `SELF_HOSTING.md` (cache/toolkit services paragraph),
  `packages/actions.md` gap list.

## Tests

* `crates/bgh-actions/tests/it/cache.rs`: legacy protocol round trip
  (miss, reserve/409, out-of-order chunks, commit, exact/prefix/version
  matching, HEAD/Range download, tampered URL, size mismatch, auth incl.
  token dead after job completion); twirp CacheService + Blob conformance
  (Put Block, Put Block List incl. invalid list and block id, Put Blob
  missing header, immutability 409, `x-ms-range`, 416, read URL can't
  write, twirp errors); twirp ArtifactService (scp backend ids, create /
  upload / finalize / list / signed URL / delete, REST visibility,
  already_exists, permission_denied, invalid name); ref scoping (feature
  branch reads main, main can't read feature, same key per scope); LRU
  eviction + up-front size refusal + expiry; REST shapes/filters/sort/
  pagination/delete/usage/policy/404 for strangers; org usage; job-token
  `actions:` permissions; **end to end** with the shell executor:
  `actions/cache` misses and saves on run 1 (post step), exact hit on run 2
  (no save), restore-key partial hit on run 3, `cache/restore` miss outputs,
  `cache/save` with no files, runtime env present.
* Unit: key validation, LIKE escaping, ranges, block lists/ids, twirp JSON
  decoding, artifact names, content ranges, cache version hash, env URLs.
* Real `gh` via `scripts/gh-compat.sh --filter cache` (5/5 pass).

## Deviations / known gaps

* No network-dependent run of `actions/setup-node` with `cache: npm` or a
  JS fixture using `@actions/cache` (no npm registry access here); the
  wire formats are covered at protocol level as allowed by the plan.
  `@actions/cache` picks the legacy protocol on any host other than
  github.com / *.ghe.com / *.localhost, which is what self-hosted installs
  use; the twirp v2 service is there for `*.localhost` setups and future
  clients.
* `@actions/artifact` v2 refuses GHES-like hosts client-side, so the twirp
  ArtifactService only helps clients that skip that check; the native
  upload/download-artifact interception remains the main path. The legacy
  v1 artifact pipeline API (`_apis/pipelines/...`, upload-artifact@v3) is
  not implemented.
* Cache `DeleteCacheEntry` / `ListCacheEntries` twirp methods (newer
  protos) answer `bad_route`.
* Uploaded artifact digests from `FinalizeArtifact.hash` are compared and
  only logged on mismatch.
