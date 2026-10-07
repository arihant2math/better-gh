-- #241: commit-order watermarks of the append-only id logs
-- (bgh_core::seqlog, SYNC_PROTOCOL.md §2). Writers no longer serialize on
-- an advisory lock; readers consume `sync_actions` / `event_outbox` only up
-- to the watermark: every id <= it has a committed row (real, or a '!gap'
-- filler for an id burned by a rollback).
CREATE TABLE log_watermarks (
    log  TEXT PRIMARY KEY,
    id   BIGINT NOT NULL DEFAULT 0
);
-- Rows written so far were committed in id order (advisory lock), so the
-- current heads are valid watermarks.
INSERT INTO log_watermarks (log, id)
     SELECT 'sync_actions', coalesce(max(id), 0) FROM sync_actions
     UNION ALL
     SELECT 'event_outbox', coalesce(max(id), 0) FROM event_outbox;
