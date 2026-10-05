-- Core schema: sync log, job queue, audit log, site settings.

-- Append-only change log for the local-first sync engine.
-- Written via bgh_core::sync::record in the same transaction as the change.
CREATE TABLE sync_actions (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- 'repo:{id}' | 'user:{id}' | 'org:{id}'
    scope       TEXT NOT NULL,
    model       TEXT NOT NULL,
    model_id    BIGINT NOT NULL,
    action      CHAR(1) NOT NULL CHECK (action IN ('I', 'U', 'D')),
    data        JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX sync_actions_scope_idx ON sync_actions (scope, id);

-- Postgres-backed job queue (bgh_core::jobs).
CREATE TABLE jobs (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind          TEXT NOT NULL,
    payload       JSONB NOT NULL DEFAULT '{}',
    run_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts      INTEGER NOT NULL DEFAULT 0,
    max_attempts  INTEGER NOT NULL DEFAULT 10,
    locked_at     TIMESTAMPTZ,
    locked_by     TEXT,
    last_error    TEXT,
    -- Set when attempts are exhausted; the row is kept for inspection.
    failed_at     TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX jobs_ready_idx ON jobs (run_at, id) WHERE failed_at IS NULL;
CREATE INDEX jobs_kind_idx ON jobs (kind);

CREATE TABLE audit_log (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    actor_id     BIGINT REFERENCES users (id) ON DELETE SET NULL,
    -- Kept even if the actor is deleted.
    actor_login  TEXT,
    -- Dotted GitHub-style action, e.g. 'repo.create', 'org.add_member'.
    action       TEXT NOT NULL,
    -- 'user' | 'org' | 'repo' | 'team' | 'token' | 'site' ...
    target_type  TEXT,
    target_id    BIGINT,
    -- Owning organization / repository for filtering (no FK: rows outlive them).
    org_id       BIGINT,
    repo_id      BIGINT,
    data         JSONB NOT NULL DEFAULT '{}',
    ip           TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_created_idx ON audit_log (created_at DESC);
CREATE INDEX audit_log_org_idx ON audit_log (org_id, created_at DESC) WHERE org_id IS NOT NULL;
CREATE INDEX audit_log_repo_idx ON audit_log (repo_id, created_at DESC) WHERE repo_id IS NOT NULL;
CREATE INDEX audit_log_actor_idx ON audit_log (actor_id, created_at DESC);
CREATE INDEX audit_log_action_idx ON audit_log (action);

CREATE TABLE site_settings (
    key         TEXT PRIMARY KEY,
    value       JSONB NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
