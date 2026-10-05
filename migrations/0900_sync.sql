-- Sync engine (bgh-sync): see docs/SYNC_PROTOCOL.md.

-- The client transaction (X-Client-Tx) that caused the action, echoed in deltas.
ALTER TABLE sync_actions ADD COLUMN IF NOT EXISTS tx UUID;
-- Compaction finds the retention cutoff by time.
CREATE INDEX IF NOT EXISTS sync_actions_created_brin ON sync_actions USING BRIN (created_at);
-- "Keep latest per model" compaction.
CREATE INDEX IF NOT EXISTS sync_actions_model_idx ON sync_actions (scope, model, model_id, id);

-- Singleton row: lowest sync id still guaranteed to be in the log.
-- Clients whose lastSyncId < min_retained_id - 1 must rebootstrap.
CREATE TABLE sync_meta (
    singleton        BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    min_retained_id  BIGINT NOT NULL DEFAULT 0,
    compacted_at     TIMESTAMPTZ
);
INSERT INTO sync_meta DEFAULT VALUES;

-- Membership rows need a stable id for the `membership` sync model.
ALTER TABLE org_members ADD COLUMN IF NOT EXISTS id BIGINT GENERATED ALWAYS AS IDENTITY;
CREATE UNIQUE INDEX IF NOT EXISTS org_members_id_key ON org_members (id);

-- Indexes used by the compact-shape loaders (bgh_core::sync::shapes).
CREATE INDEX IF NOT EXISTS pr_requested_reviewers_pull_idx ON pr_requested_reviewers (pull_id);

-- Sync timestamp format (`2024-01-01T00:00:00Z`), same as the REST API.
-- STABLE (like to_char) so the planner inlines it.
CREATE OR REPLACE FUNCTION bgh_ts(t TIMESTAMPTZ) RETURNS TEXT
    LANGUAGE sql STABLE PARALLEL SAFE
    AS $$ SELECT to_char(t AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"') $$;
