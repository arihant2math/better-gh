-- P27: Actions cache entries (actions/cache, @actions/cache v1 and v2
-- protocols, REST /actions/caches) and per-repository cache size policies.
-- Archives live in {data_dir}/actions/caches/{id}.

CREATE TABLE actions_caches (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id          BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    key              TEXT NOT NULL,
    version          TEXT NOT NULL,
    -- Scope: the ref of the run that created it (refs/heads/x, refs/pull/1/merge).
    ref              TEXT NOT NULL,
    size_in_bytes    BIGINT NOT NULL DEFAULT 0,
    -- Reserved entries are invisible until the upload is committed.
    committed        BOOLEAN NOT NULL DEFAULT false,
    run_id           BIGINT REFERENCES actions_runs (id) ON DELETE SET NULL,
    job_id           BIGINT REFERENCES actions_jobs (id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_accessed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (size_in_bytes >= 0)
);

-- Entries are immutable: one per (scope, key, version).
CREATE UNIQUE INDEX actions_caches_entry_key ON actions_caches (repo_id, ref, key, version);
-- Restore lookups: exact and prefix matches within a scope.
CREATE INDEX actions_caches_lookup_idx ON actions_caches (repo_id, ref, version, key text_pattern_ops)
    WHERE committed;
-- REST lists / LRU eviction per repository.
CREATE INDEX actions_caches_repo_accessed_idx ON actions_caches (repo_id, last_accessed_at, id);
-- Expiry sweep.
CREATE INDEX actions_caches_accessed_idx ON actions_caches (last_accessed_at);
CREATE INDEX actions_caches_run_idx ON actions_caches (run_id);
CREATE INDEX actions_caches_job_idx ON actions_caches (job_id);

CREATE TABLE actions_cache_policies (
    repo_id                   BIGINT PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    repo_cache_size_limit_in_gb INTEGER NOT NULL CHECK (repo_cache_size_limit_in_gb > 0),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now()
);
