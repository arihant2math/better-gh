-- P1: fork-safe git maintenance and scheduled housekeeping.

-- Admin maintenance operations: `prune` (forced, repositories without
-- dependents only) and `dissociate` (make a fork self-contained).
ALTER TABLE repo_maintenance_runs DROP CONSTRAINT repo_maintenance_runs_operation_check;
ALTER TABLE repo_maintenance_runs ADD CONSTRAINT repo_maintenance_runs_operation_check
    CHECK (operation IN ('gc', 'fsck', 'repack', 'recalculate_size',
                         'recalculate_languages', 'prune', 'dissociate'));

-- State of the scheduled `repos.maintenance` service per repository.
CREATE TABLE repo_maintenance (
    repo_id       BIGINT PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    -- Last scheduled run (incremental or full) that touched the repository.
    last_run_at   TIMESTAMPTZ,
    -- Last full repack (scheduled, or an admin gc/repack).
    last_full_at  TIMESTAMPTZ,
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'succeeded', 'failed', 'skipped')),
    error         TEXT,
    pack_count    BIGINT NOT NULL DEFAULT 0,
    loose_count   BIGINT NOT NULL DEFAULT 0,
    has_alternates BOOLEAN NOT NULL DEFAULT false,
    has_dependents BOOLEAN NOT NULL DEFAULT false,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Admin listing (failed first / most recent) and due-repo selection.
CREATE INDEX repo_maintenance_status_idx ON repo_maintenance (status, updated_at DESC);
CREATE INDEX repo_maintenance_full_idx ON repo_maintenance (last_full_at NULLS FIRST);
