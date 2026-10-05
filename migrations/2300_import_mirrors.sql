-- P11: repository import from a URL and pull mirrors.

-- Upstream URL of a pull mirror (credentials never included); NULL for
-- regular repositories. Mirrors refuse pushes and API ref writes.
ALTER TABLE repositories ADD COLUMN mirror_url TEXT;

-- One import per repository (the latest attempt; retry reuses the row).
CREATE TABLE repo_imports (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id             BIGINT NOT NULL UNIQUE REFERENCES repositories (id) ON DELETE CASCADE,
    source_url          TEXT NOT NULL,
    -- `username\0secret` sealed with the server key (bgh_core::secretbox).
    enc_credentials     BYTEA,
    mirror              BOOLEAN NOT NULL DEFAULT false,
    include_lfs         BOOLEAN NOT NULL DEFAULT false,
    status              TEXT NOT NULL DEFAULT 'queued'
                        CHECK (status IN ('queued', 'importing', 'complete', 'failed', 'cancelled')),
    phase               TEXT NOT NULL DEFAULT 'queued',
    objects_received    BIGINT NOT NULL DEFAULT 0,
    objects_total       BIGINT NOT NULL DEFAULT 0,
    bytes_received      BIGINT NOT NULL DEFAULT 0,
    lfs_received        BIGINT NOT NULL DEFAULT 0,
    lfs_total           BIGINT NOT NULL DEFAULT 0,
    error               TEXT,
    attempts            INT NOT NULL DEFAULT 0,
    creator_id          BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at        TIMESTAMPTZ
);
CREATE INDEX repo_imports_creator_idx ON repo_imports (creator_id);

CREATE TABLE repo_mirrors (
    repo_id             BIGINT PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    url                 TEXT NOT NULL,
    enc_credentials     BYTEA,
    interval_minutes    INT NOT NULL DEFAULT 480 CHECK (interval_minutes BETWEEN 10 AND 43200),
    enabled             BOOLEAN NOT NULL DEFAULT true,
    include_lfs         BOOLEAN NOT NULL DEFAULT false,
    creator_id          BIGINT REFERENCES users (id) ON DELETE SET NULL,
    last_sync_at        TIMESTAMPTZ,
    next_sync_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_status         TEXT NOT NULL DEFAULT 'pending'
                        CHECK (last_status IN ('pending', 'success', 'failed')),
    last_error          TEXT,
    consecutive_failures INT NOT NULL DEFAULT 0,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Scheduler: due mirrors.
CREATE INDEX repo_mirrors_due_idx ON repo_mirrors (next_sync_at) WHERE enabled;
-- Admin: failing mirrors.
CREATE INDEX repo_mirrors_failed_idx ON repo_mirrors (last_sync_at DESC, repo_id)
    WHERE last_status = 'failed';
CREATE INDEX repo_mirrors_creator_idx ON repo_mirrors (creator_id);
