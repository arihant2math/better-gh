Integration: landed
P65 secret scanning + push protection: new crate bgh-security, migration 7700, REST + webhooks, web Security tab/alerts/unblock/settings; full gate green.

# P65 — Secret scanning and push protection: status

Branch `bgh/p65-secret-scanning`. Scope of PHASE4_PLAN §P65 done (no §5
quick fixes were assigned to P65).

## Backend (new crate `bgh-security`)

* `patterns`: one `regex::bytes::RegexSet` per engine (linear time, no
  backtracking), then per-hit extraction. 31 provider patterns (this
  server's `bghp_`/`bgho_`/`bghs_`, GitHub `ghp_`/`gho_`/`ghs_`/`ghr_`/
  `github_pat_`, AWS key id + secret, Google API key / OAuth secret / GCP
  key id, Azure storage / AAD secret, Slack token + webhook, Stripe live /
  restricted / test, PEM private keys, npm, PyPI, RubyGems, Docker,
  GitLab, SendGrid, Shopify, OpenAI, Anthropic, Vault) and 5 non-provider
  patterns (basic/bearer auth headers, Postgres/MySQL/MongoDB URLs; opt-in
  per repo via `secret_scanning_non_provider_patterns`). Placeholders
  (`…EXAMPLE`, `XXXXXXXX`) and binary blobs (NUL in the first 8 KB) are
  skipped. Custom patterns compile with a size limit, reject
  empty-matching regexes and are capped at 1000 chars.
* `scan`: commits → `diff-tree --stdin` (P23's `GitCli::changed_files`)
  → distinct blobs ≤ `max_blob_kb` → `cat-file --batch` in 32 MB batches
  → matching on the blocking pool. Each finding fans out to every
  (commit, path) that introduced the blob.
* `store`: alerts dedupe on `(repo, secret_type, sha256(secret))`
  (secret sealed with `bgh_core::secretbox`), numbers per repo under an
  advisory lock, locations deduped; emits `SecretScanningAlert` /
  `SecretScanningAlertLocationCreated`. A new alert for a secret that was
  pushed through a bypass records `push_protection_bypassed*`;
  `used_in_tests` / `false_positive` bypasses close it with that
  resolution, `will_fix_later` leaves it open.
* `push`: push protection as a receive-pack object check (`ObjectCheck`,
  see below), wired into HTTP (`git_http::receive_pack`) and SSH
  (`ssh/exec.rs`) through `push::combine` (additive to any ruleset check).
  Scans only `new… --not --all` in quarantine, push-protected patterns
  only (providers + custom patterns with `push_protection`), bounded by
  `max_blob_kb` and `push_scan_timeout_secs` (fails open with a warning;
  errors fail open and are logged). Rejection is GitHub's GH013 "GITHUB
  PUSH PROTECTION" report with locations and an unblock URL per secret
  (`/{o}/{r}/security/secret-scanning/unblock-secret/{placeholder}`, row in
  `secret_scanning_push_blocks`). A secret is allowed when the same pusher
  bypassed it within 3 h, or an alert of the repo with the same hash was
  resolved as false positive / used in tests / won't fix.
* `jobs`: `security.scan_history` (all refs; on enable, custom pattern
  create/edit, manual scan, backfill service), `security.scan_push`
  (listener `security.secret_scanning` on `Push`, effect-claimed; range =
  new tips `--not` old tips `--exclude=<pushed refs> --glob=refs/*`), and
  service `security.backfill` (advisory-lock leader, every 60 s: repos with
  scanning effectively on and no backfill yet — covers site-wide enable).
  Each scan is a `secret_scanning_scans` row.
* `settings`: effective settings = repo toggles (`repo_security_settings`)
  ∨ site `secret_scanning.enable_all` / `push_protection_all`, gated by
  `available`; push protection requires scanning.

## Endpoints

| Endpoint | Notes |
|---|---|
| `GET /repos/{o}/{r}/secret-scanning/alerts` | Repo admins (403 readers, 401 anon, 404 "Secret scanning is disabled on this repository."). `state`, `secret_type`, `resolution` (csv), `sort=created\|updated`, `direction`; Link pagination; 422 on bad filters. |
| `GET/PATCH /repos/{o}/{r}/secret-scanning/alerts/{n}` | PATCH `state` (required), `resolution` (required for resolved; `false_positive\|wont_fix\|revoked\|used_in_tests`), `resolution_comment` (≤ 280). Emits `resolved`/`reopened`. |
| `GET /repos/{o}/{r}/secret-scanning/alerts/{n}/locations` | `{type: "commit", details: {path, start/end line/column, blob_sha/url, commit_sha/url}}`, paginated. |
| `POST /repos/{o}/{r}/secret-scanning/push-protection-bypasses` | `{reason, placeholder_id}` → `{reason, expire_at, token_type}`; pusher or admin (write access needed). |
| `GET /repos/{o}/{r}/secret-scanning/scan-history` | `incremental_scans`, `backfill_scans`, `custom_pattern_backfill_scans`, `pattern_update_scans` (latest 20 each). |
| `GET /orgs/{org}/secret-scanning/alerts` | Org owners (403 members, 404 others), repos with scanning on; alerts carry `repository`. |
| `GET/PATCH /repos/{o}/{r}` | `security_and_analysis` shown to admins (effective status), PATCH toggles `secret_scanning`, `secret_scanning_push_protection`, `secret_scanning_non_provider_patterns` (422 on bad status); enabling queues a backfill; audited `repo.security_and_analysis`. |
| `GET /_bgh/repos/{o}/{r}/secret-scanning/settings` | Effective toggles, `enforced_by_site`, `open_alerts`. |
| `POST /_bgh/repos/{o}/{r}/secret-scanning/scan` | Queue a history scan (202). |
| `GET /_bgh/repos/{o}/{r}/secret-scanning/push-blocks/{placeholder}` | Unblock page data (pusher or admin; others 404). |
| `GET/POST /_bgh/{repos/{o}/{r},orgs/{org}}/secret-scanning/custom-patterns`, `PATCH/DELETE …/{id}` | Repo admins / org owners. Validation 422 (name, regex, test string must match). Create/edit queue `custom_pattern_backfill` scans; delete resolves open alerts as `pattern_deleted`. Audited. |
| `POST /_bgh/secret-scanning/custom-patterns/test` | Dry run `{valid, error, matches: [{start, end, text}]}` (char offsets). |
| `GET /_bgh/secret-scanning/patterns` | Built-in pattern list. |

Webhooks: `secret_scanning_alert` (`created`, `resolved`, `reopened`; no
`secret` in the payload) and `secret_scanning_alert_location` (`created`);
both removed from P10's NOT_PRODUCIBLE_YET with samples added.

Site admin: settings section `secret_scanning` (`available`,
`enable_all`, `push_protection_all`, `max_blob_kb` = 1024,
`push_scan_timeout_secs` = 20; positive values validated).

## Tables (migration `7700_secret_scanning.sql`)

`repo_security_settings`, `secret_scanning_custom_patterns`,
`secret_scanning_alerts`, `secret_scanning_locations`,
`secret_scanning_push_blocks`, `secret_scanning_scans` (all FKs and list
shapes indexed).

## Shared-code changes (additive)

* `bgh-core`: `secret_scanning.rs` (alert/location rows + REST shapes,
  used by the API and bgh-notify), `events.rs` two variants,
  `settings.rs` `SecretScanningSettings` section, `models::api::Repository`
  `security_and_analysis` (skipped when `None`).
* `bgh-git`: **P23's object-check mechanism copied verbatim** from
  `origin/bgh/p23-rulesets` (not yet landed): `smart_http`
  `ObjectCheck`/`QuarantineEnv`/`HookVerdict`, the FIFO block of
  `PRE_RECEIVE_HOOK`, `serve_object_check`, `mkfifo`, hook-mode repair,
  multi-line `rejection_report`, `pushed.rs`, `GitCli::is_ancestor_with`,
  `libc` dep. Identical text, so merging P23 later is clean or a
  take-either-side conflict. `REPO_CONFIG` untouched.
* `bgh-notify`: `payloads/secret_scanning.rs` + two match arms.
* `bgh-repos`: depends on `bgh-security`; push wiring (HTTP + SSH),
  `security_and_analysis` in PATCH and full repo JSON.
* `bgh-server`: mounts `bgh-security`.

## Web

Lazy chunk `pages/security/SecurityPage` serves `/:owner/:repo/security`
(overview), `/security/secret-scanning` (list: open/closed, type filter),
`/security/secret-scanning/:number` (detail: masked secret with reveal /
copy, locations linking to blob and commit, close-as with reason +
comment, reopen, bypass info) and `/security/secret-scanning/unblock-secret/:placeholder`
(bypass page). Repo settings section "Code security" (toggles, enforced
badges, scan now, scan history, custom patterns with live dry run). Org
settings "Secret scanning" (org patterns, org alerts). Site admin section
"Secret scanning". `SECURITY_NAV` in `pages/security/shared.tsx` is the
list P66 extends. Mocks in `web/src/mock/extra/secretScanning.ts`.
Initial bundle 144.5 KB gzip (+0.3). Verified with Playwright against the
mock (25 checks, light/dark) and against a real server (blocked push →
unblock page → bypass → push → alerts list/detail/settings).

## Tests

`cargo test -p bgh-security`: unit (patterns, batch parsing, rev args,
settings) and `tests/it/{push_protection,alerts,custom_patterns}.rs`:
AWS key push blocked with GH013 text, bypass (REST) then push passes and
the alert exists (bypassed, open / closed for used-in-tests), per-user
bypasses, site-wide enforcement, history scan finds a planted (deleted)
key, alert shapes / filters / pagination / resolve / reopen / locations
/ permissions, org list, webhook payload, custom pattern validation, dry
run, backfill, push protection and deletion, org patterns.

Benchmark (`bench.rs`, ignored): 100 MB push (200 × 512 KB text files,
one AWS key) in a **debug** build: 13.2 s blocked with push protection vs
11.2 s without, i.e. ≈ 2 s scan overhead (~50 MB/s debug; the matcher
alone is much faster in release, `patterns::tests::throughput_100mb`).

## Known gaps

* No validity checks (`validity` is always `unknown`), no
  `publicly_leaked`/`multi_repo`, no delegated bypass reviews.
* Pattern edits don't resolve alerts that stop matching (`pattern_edited`
  is accepted in filters only).
* `pattern_update_scans` stays empty (built-in pattern changes don't
  trigger rescans).
* Wiki pushes are not scanned.
