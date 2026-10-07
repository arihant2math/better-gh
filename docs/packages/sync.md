# B8 sync (`bgh-sync`) — status

**Done.** Implements `docs/SYNC_PROTOCOL.md` on the server: bootstrap,
partial sync, WebSocket replay + live fan-out, permission filtering and
revocation, X-Client-Tx capture / `X-Bgh-Sync-Id` / idempotency, log
compaction, and the shared compact-shape builders every domain crate uses
for its sync payloads. Branch `bgh/sync`, merged with the integration
branch.

## Endpoints

| Route | Notes |
|-------|-------|
| `GET /_bgh/sync/bootstrap[?scopes=...]` | `RequireUser`. Default scope set (user, org memberships, repos with explicit access incl. parent-team grants and org base permission) or explicit scopes; unreadable / unknown / malformed scopes in `denied`. One `REPEATABLE READ, READ ONLY` snapshot (`jit = off`), `lastSyncId = max(id)` in it. Models aggregated in Postgres (`string_agg` of `row_to_json`/`json_build_object`) and spliced as text; issue sets ≥ 2000 rows are split over up to 3 idle helper connections that import the snapshot (`pg_export_snapshot` / `SET TRANSACTION SNAPSHOT`). Bodies ≥ 64 KiB are compressed in the handler (brotli q2 / gzip level 1, blocking pool); smaller ones by the global layer. `Cache-Control: no-store`. |
| `GET /_bgh/sync/partial?model=comment,review,issueEvent&issue=ID` / `?model=issue&id=ID` | `MaybeUser` (public repos readable anonymously); 404 without read access; issue row includes `body`; `user` refs; pending reviews only for their author; 422 for bad `model`/missing ids. |
| `GET /_bgh/sync/ws[?v=1]` | Session cookie (Origin must match `BGH_BASE_URL`, the request host or `BGH_SYNC_ALLOWED_ORIGINS`) or token. `hello`, `sub`/`unsub`/`ping`, replay in batches of ≤ 500, `ready`, live `delta`/`batch` (~10 ms coalescing) with `tx` echo and `refs.user`, `revoke`, `rebootstrap` (`too_old`/`schema`, close 4009), 4001 for anonymous/bad credentials/sign-out, protocol pings every 30 s, idle close after 90 s, `slow_consumer` + 1013 on queue overflow. |

## Architecture

* `bgh_core::sync::shapes` — the single place that builds compact rows
  (`Model` enum, `load`, `load_one`, `load_joined`, `viewer_repos`,
  `referenced_users`) plus `Tx` helpers for domain crates:
  `sync_model(s)`, `sync_issue(id, action, body_changed)`, `sync_delete`,
  `sync_user` (user + org scopes), `sync_viewer_repo` (D when access lost).
  `SyncModel` is in the prelude. Documented in BACKEND_PATTERNS.md §8a.
* `bgh_core::sync::context` — task-local request context
  (`RequestSync`): `record` stores the request's `X-Client-Tx`
  automatically; committed ids are noted for `X-Bgh-Sync-Id`.
* `bgh_core::sync::record` takes `pg_advisory_xact_lock(SYNC_LOCK)` in the
  same statement as the insert (one round trip) so ids commit in order.
  `Tx` collects its actions and writes them with `sync::record_all` (one
  statement) right before committing, so the lock is never held while a
  transaction still takes row locks (this removed lock-order deadlocks
  between request handlers and event listeners).
* Extension models `reviewComment`, `checkRun`, `checkSuite`,
  `commitStatus` (delta-only, SYNC_PROTOCOL.md §3.2) and the issue / event
  extension fields are part of `shapes`.
* The `sync.access` listener also turns `Event::SessionEnded` into a
  sign-out signal (sockets of that session close with 4001).
* `bgh_sync::http_middleware` (mounted in bgh-server for every route):
  context, `X-Bgh-Sync-Id`, idempotency per `(user, tx)` in Redis
  (`SET NX` pending marker → stored `{status, headers, body}` for 24 h;
  5xx/408/429/401 not stored; in-flight duplicate → 429 `Retry-After: 1`;
  replay sets `Idempotent-Replayed: true`).
* `bgh_sync::hub` — one Redis `PSUBSCRIBE {prefix}sync:*` per process,
  bounded per-socket queues (256), in-order delivery with DB gap filling
  (non-contiguous ids, lost publishes via 1 s head poll, reconnects),
  subscribe-at-`L` handshake (replay `(since, L]`, live `> L`), permission
  rechecks (access events/deltas, sign-out messages, 5-minute sweep).
* `bgh_sync::compact` — `sync.compact` job, self-rescheduling
  (`BGH_SYNC_COMPACT_INTERVAL_SECS`, default 3600; first scheduled on the
  first bootstrap/socket of a process). Truncate mode advances
  `sync_meta.min_retained_id` before deleting (replays recheck it after
  finishing); `BGH_SYNC_KEEP_LATEST=1` keeps the latest action per row.
  Retention `BGH_SYNC_RETENTION_HOURS` (168).

## Migrations

`0900_sync.sql`: `sync_actions.tx UUID`, BRIN on `sync_actions.created_at`,
`(scope, model, model_id, id)` index, `sync_meta` singleton
(`min_retained_id`, `compacted_at`), `org_members.id` (identity, unique —
the `membership` model needs a row id), `pr_requested_reviewers(pull_id)`
index, SQL function `bgh_ts(timestamptz)` (sync timestamp format, STABLE so
it inlines).

## Shared-code changes (all additive)

* `bgh-core`: `sync::{context, shapes}` modules; `SyncRecord.tx`
  (`Option<Uuid>`, skipped when `None`); `record_with_tx`; `SYNC_LOCK`,
  `SCHEMA_VERSION`, `ACCESS_CHANNEL`, `signal_signed_out`; `notify` notes
  committed ids; `Event::AccessChanged { repo_id, org_id, user_id }`;
  `auth::destroy_session` / `destroy_user_sessions` signal sign-outs to the
  hubs; prelude exports `SyncModel`.
* `bgh-server`: mounts `bgh_sync::http_middleware` (inside the auth-header
  layer, outside compression).
* `bgh-repos`: repo create/push/delete now record model `repo` through the
  shapes (`sync_model`/`sync_delete`) instead of the non-spec `repository`
  snake_case JSON; create also records the creator's `viewerRepo`.
  `json::repo_sync_json` is kept (unused) for compatibility.
* Docs: BACKEND_PATTERNS.md §8/§8a, ARCHITECTURE.md "Sync engine",
  SYNC_PROTOCOL.md server notes (no wire changes; `schemaVersion` stays 1).

## Benchmark (bootstrap, one repo with 10k issues)

`cargo test --release -p bgh-sync --test bench -- --ignored --nocapture`
seeds 10k issues (2k PRs; 2 labels, ½ assigned, ¼ with reactions, reviewers,
reviews, check runs) and times the full in-process request. This container:
4 vCPUs (Firecracker VM, Postgres 16 on the same box, test pool of 5
connections → 2–3 helper chunks), medians of 7 runs:

| encoding | size | median (min) |
|----------|------|--------------|
| identity | 4.26 MB | 133–149 ms (107 ms) |
| br (q2) | 238 KB | 143 ms (116 ms) |
| gzip (level 1) | 460 KB | 129–160 ms (121 ms) |
| partial (1 issue) | — | 6–8 ms |

History: 850 ms initially — 640 ms of it Postgres LLVM JIT compiling the
wide projection (now `SET LOCAL jit = off` in sync snapshots; consider
`jit = off` server-wide), then a flat single-pass issue query with
hash-joined child aggregates and `row_to_json` (≈2× cheaper than
`json_build_object` here), set-based PR checks, parallel snapshot chunks,
and fast handler-side compression (the generic layer's defaults added
~50 ms on 4 MB).

## Tests (`cargo test -p bgh-sync`)

* `bootstrap.rs`: exact shapes of every model (user, org, membership, team,
  repo, viewerRepo, label, milestone, issue, PR fields incl. reviewDecision
  and checks, notification), default scope set (owner, collaborator, org
  base permission incl. `none`, parent-team grants), explicit scopes +
  `denied`, token scopes, partial sync (comment/review/issueEvent shapes,
  pending-review privacy, 404s, 422s), Tx helpers record exactly the
  bootstrap rows, gzip/br.
* `shapes.rs`: drives the REST APIs of accounts, repos, issues, pulls and
  notify and checks that the latest delta of every row equals its
  bootstrap / partial-sync / shape row, for every model.
* `ws.rs`: 4001 unauthenticated, cross-origin cookie rejected, replay →
  ready → live delta with `tx` echo and `refs`, batching, deletes,
  unsub, errors, resume from `since` and adding scopes, denied scopes,
  revoke on `AccessChanged` and on a visibility delta, rebootstrap
  (too_old/schema, 4009), lost/out-of-order publishes filled from the log,
  one hub for many sockets, sign-out closes the session's sockets (4001)
  (also when revoked through `DELETE /_bgh/sessions/{id}`),
  slow consumer dropped.
* `middleware.rs`: tx recorded on every action, `X-Bgh-Sync-Id`,
  idempotent replay (incl. 4xx), per-user keys, in-flight 429.
* `compact.rs`: truncate / keep-latest / empty log, job rescheduling.
* `bench.rs` (ignored): the benchmark above.

## Known gaps / notes for other packages

* issues, pulls, accounts, notify, admin and repos record through the §8a
  helpers (`tests/shapes.rs` checks every model end to end); bgh-projects
  and bgh-wiki are being converted by their package. Raw `tx.sync` remains
  for models no client loads (`release`, `release_asset`, `workflow_run`,
  `workflow_job`, `ruleset`, `branch_protection`, `notificationSettings`).
* Secret teams are part of the `org:{id}` scope, so every org member
  receives them (GitHub hides them from non-members).
* Revocation is asynchronous: deltas committed in the few milliseconds
  between losing access and the recheck can still reach that socket.
* `keep_latest` compaction can leave a client with a stale lazy `body` if
  the body change was compacted away and the client already loaded it.
* Site admins' default scope set contains only explicit grants (they can
  still subscribe to any repo explicitly).
* `authorAssociation` doesn't distinguish `FIRST_TIME_CONTRIBUTOR`.
* `avatarUrl` is `""` without a custom avatar (client renders initials).
* Bootstrap includes all not-done notifications (no cap yet).
