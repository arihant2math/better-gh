-- P9: durable event delivery (transactional outbox + per-listener cursors).

-- Every domain event, written in the emitting transaction. Ids are
-- allocated under the sync advisory lock (held until commit), so they
-- become visible in id order and a consumer reading `id > cursor` never
-- skips a row that commits later.
CREATE TABLE event_outbox (
    id          BIGSERIAL PRIMARY KEY,
    -- `Event::name()`, e.g. `issue_opened`.
    kind        TEXT NOT NULL,
    -- The serialized `Event` (`{"type": kind, ...}`).
    payload     JSONB NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX event_outbox_created_idx ON event_outbox (created_at);

-- One row per durable listener (`reg.on_event` name): the last event it
-- fully processed, and the lease that makes it the only consumer across
-- processes.
CREATE TABLE event_listener_cursors (
    listener     TEXT PRIMARY KEY,
    last_id      BIGINT NOT NULL DEFAULT 0,
    lease_owner  UUID,
    lease_until  TIMESTAMPTZ,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Idempotency keys for at-least-once delivery: a redelivered event must
-- not produce a second webhook delivery, notification or activity row.
ALTER TABLE webhook_deliveries
    ADD COLUMN event_id BIGINT,
    ADD COLUMN event_seq INTEGER;
CREATE UNIQUE INDEX webhook_deliveries_event_idx
    ON webhook_deliveries (hook_id, event_id, event_seq) WHERE event_id IS NOT NULL;

-- Generic idempotency receipts (`bgh_core::events::claim_effect`): one row
-- per (listener, event, side effect), claimed in the transaction that
-- performs the effect (a notification activity's rows + email job, a
-- workflow trigger job, ...), so a redelivered event finds it taken.
CREATE TABLE event_receipts (
    listener    TEXT NOT NULL,
    event_id    BIGINT NOT NULL,
    seq         INTEGER NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (listener, event_id, seq)
);
CREATE INDEX event_receipts_created_idx ON event_receipts (created_at);

ALTER TABLE activity_events
    ADD COLUMN event_id BIGINT,
    ADD COLUMN event_seq INTEGER;
CREATE UNIQUE INDEX activity_events_event_idx
    ON activity_events (event_id, event_seq) WHERE event_id IS NOT NULL;
