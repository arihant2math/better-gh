# P9 durable-events — status

**Done.** Branch `bgh/p09-durable-events`. Scope: `docs/PHASE4_PLAN.md` §P9
(no §5 quick fixes are assigned to P9).

## What changed

* **Transactional outbox** (`bgh_core::outbox`, migration
  `2100_event_outbox.sql`). `Tx::commit` appends queued events to
  `event_outbox(id bigserial, kind, payload jsonb, created_at)` in the
  caller's transaction, right before commit, under the existing
  `sync::SYNC_LOCK` advisory lock, so ids become visible in id order (a
  consumer reading `id > cursor` never skips a later-committing lower id).
  The same statement `pg_notify('bgh_events')`; after commit the bus wakes
  local consumers and broadcasts in-process. `tx.emit` is unchanged.
* **Direct `state.events.emit(e)`** (no `Tx`; a few production sites and
  many tests) stays synchronous and source-compatible: it broadcasts
  in-process and hands the event to a per-bus background writer that
  appends to the outbox in call order (batched).
  `EventBus::flush().await` waits for it (used on shutdown and by
  `outbox::wait_caught_up`). `EventBus::new()` (no DB) still exists;
  `AppState::new` now builds `EventBus::durable(db)`.
* **Durable consumers.** `reg.on_event` is unchanged; each listener is
  now consumed by `outbox::start_listeners` (also
  `registry::start_listeners`, async; `registry::spawn_listeners` kept as
  a sync wrapper):
  * cursor per listener in `event_listener_cursors(listener, last_id,
    lease_owner, lease_until, updated_at)`; a new listener's cursor starts
    at the current head (no replay of history);
  * batches of 200 in id order; the cursor advances after each batch
    (at-least-once); no drops on lag;
  * one consumer per listener across processes via a 30 s lease on the
    cursor row (renewed while working; other processes poll every 2 s and
    take over when it expires or is released); cursor commits are fenced
    by `lease_owner`;
  * wakeups: in-process `watch` channel + `LISTEN bgh_events` on a
    dedicated connection (outside the pool) + 2 s poll fallback;
  * failing/panicking handlers are retried 3 times (50/100 ms backoff),
    then the event is skipped with an error log; undecodable payloads
    (unknown variant from another binary version) are skipped with a
    warning;
  * handlers run with a task-local event context:
    `events::current_event_id()`, `events::effect_key()` (`(event_id, n)`
    per side effect), `events::claim_effect(&mut tx)` (generic receipt in
    `event_receipts(listener, event_id, seq)`),
    `events::with_listener_event(..)` for replays.
* **Idempotent handlers** (unique keys):
  * webhook deliveries: `webhook_deliveries.event_id/event_seq`, unique
    `(hook_id, event_id, event_seq)` (seq = index of the payload the event
    maps to), `ON CONFLICT DO NOTHING`, job enqueued only on insert
    (`bgh-notify/src/webhooks/dispatch.rs`; `queue_delivery` signature
    unchanged). Global hook pings too.
  * notifications: `fanout::deliver_once` claims an `event_receipts` row
    per (event, activity) in the delivery transaction: a redelivery writes
    no notification rows, sync actions or email job. The plan's
    `(user, thread, event_id)` key is covered because all of an activity's
    (user, thread) rows commit atomically with its receipt.
    `fanout::deliver` (unkeyed) unchanged for other callers.
  * activity: `activity_events.event_id/event_seq` unique, keyed by
    `effect_key()` in `bgh-search` `record::insert`.
  * also guarded with `claim_effect`: `actions.trigger` (would start
    duplicate workflow runs) and `admin.counters` (push counter).
  * Other listeners were already idempotent (upserts, `EXISTS` checks, or
    jobs that are idempotent).
* **Pruning:** every 10 min, rows with `id <= min(cursor of registered
  listeners)` and older than `BGH_EVENT_RETENTION_DAYS` (default 7) are
  deleted, plus receipts of the same age (`outbox::prune`).
* **Consumer lag for P61:** `outbox::consumer_lag(&db) ->
  Vec<ConsumerLag { listener, last_id, head_id, lag, oldest_pending_secs,
  lease_owner, lease_until, updated_at }>`; `outbox::head(&db)`.
* **Graceful shutdown:** `bgh_server::serve(state, registry, router,
  listener, signal)` (main.rs uses it). Separate tokens for HTTP and
  background work: signal → stop accepting → await in-flight requests (up
  to `BGH_SHUTDOWN_TIMEOUT_SECS`, default 30) → flush direct emits →
  cancel background (workers finish the current job, services stop,
  consumers drain committed events for up to 15 s and release leases) →
  exit.
* **Test harness:** `TestApp` starts consumers with `start_listeners`
  (cursors exist before the first request), plus
  `app.settle_events()`, `app.stop_listeners()`, `app.start_listeners()`.

## Shared-code changes (all additive / source-compatible)

* `bgh-core`: new `outbox.rs`; `events.rs` (bus internals, task-local
  context helpers — the `Event` enum untouched); `registry.rs`
  (`Listener::call`, `start_listeners`); `db.rs` (`Tx::commit` appends to
  the outbox); `state.rs`; `config.rs` (`event_retention_days`,
  `shutdown_timeout_secs`); `testing.rs`.
* `bgh-notify` dispatch.rs / fanout.rs, `bgh-search` record.rs,
  `bgh-actions` trigger.rs (end of `on_event` only), `bgh-admin` stats.rs.
* `bgh-server`: new `serve.rs`, `main.rs` uses it; `futures` dep.

## Migrations

`2100_event_outbox.sql`: `event_outbox`, `event_listener_cursors`,
`event_receipts`, `webhook_deliveries.event_id/event_seq` (+ unique
index), `activity_events.event_id/event_seq` (+ unique index).

## Tests (`crates/bgh-server/tests/it/events.rs`)

* restart: events emitted (committed and direct) while a listener is
  stopped are delivered exactly once after it restarts;
* burst: 10 000 events (9 000 committed, 1 000 direct), none lost, in
  order, no duplicates;
* two registries (separate buses, woken only via NOTIFY) on one database:
  each event delivered once; after the lease holder stops the other takes
  over;
* handler retries then skip; consumer lag + pruning keeps unprocessed
  rows;
* redelivery (all cursors rewound): no second webhook delivery,
  notification, email job or activity row;
* SIGTERM: a request in flight when the signal arrives completes (201)
  and its webhook delivery exists when `serve` returns; leases released.

## Known gaps / notes

* Listener handlers still run sequentially per listener (as before); a
  slow handler delays that listener only.
* An event that fails 3 times is skipped (logged at error) rather than
  parked in a dead-letter table.
* Readers of `state.events.subscribe()` (tests) still get the in-memory
  broadcast only from the emitting process.
* No web UI (none in scope); P61 can export `consumer_lag` as metrics.
