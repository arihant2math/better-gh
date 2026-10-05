-- P18: metadata importer (GitHub / GHES issues, labels, milestones,
-- releases, users). Range 3000-3099.

-- Placeholder accounts for source users that could not be mapped to a
-- local account. They cannot sign in (no password, no email, refused by
-- every credential check) and display the source login. Reclaim: P51.
ALTER TABLE users
    ADD COLUMN mannequin BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN mannequin_source TEXT,          -- e.g. "github.com"
    ADD COLUMN mannequin_login TEXT;           -- login on the source

CREATE INDEX users_mannequin_idx ON users (id) WHERE mannequin;

CREATE TABLE imports (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind             TEXT NOT NULL DEFAULT 'github' CHECK (kind IN ('github')),
    api_url          TEXT NOT NULL,             -- https://api.github.com or https://ghes/api/v3
    source_repo      TEXT NOT NULL,             -- owner/name on the source
    enc_token        BYTEA,                     -- secretbox-sealed, never returned
    owner_id         BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    repo_name        TEXT NOT NULL,
    repo_id          BIGINT REFERENCES repositories(id) ON DELETE SET NULL,
    visibility       TEXT NOT NULL DEFAULT 'private'
                     CHECK (visibility IN ('public', 'private', 'internal')),
    options          JSONB NOT NULL DEFAULT '{}',   -- steps, user_map
    status           TEXT NOT NULL DEFAULT 'queued'
                     CHECK (status IN ('queued', 'running', 'waiting', 'complete', 'failed', 'cancelled')),
    step             TEXT NOT NULL DEFAULT 'git',
    cursor           JSONB NOT NULL DEFAULT '{}',   -- per-step resume point
    stats            JSONB NOT NULL DEFAULT '{}',   -- counts per kind
    error            TEXT,
    attempts         INT NOT NULL DEFAULT 0,
    resume_at        TIMESTAMPTZ,                   -- rate-limit wait
    heartbeat_at     TIMESTAMPTZ,
    created_by       BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at     TIMESTAMPTZ
);

CREATE INDEX imports_owner_idx ON imports (owner_id, id DESC);
CREATE INDEX imports_repo_idx ON imports (repo_id);
CREATE INDEX imports_created_by_idx ON imports (created_by);
CREATE INDEX imports_active_idx ON imports (status, heartbeat_at)
    WHERE status IN ('queued', 'running', 'waiting');

-- Source object -> local row. `scope` is the source host for users
-- (shared across imports, so a mannequin is reused) and `import:{id}` for
-- repository-scoped objects (issues, comments, labels, ...).
CREATE TABLE import_mappings (
    scope        TEXT NOT NULL,
    source_type  TEXT NOT NULL,
    source_id    TEXT NOT NULL,
    local_id     BIGINT NOT NULL,
    import_id    BIGINT REFERENCES imports(id) ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (scope, source_type, source_id)
);

CREATE INDEX import_mappings_import_idx ON import_mappings (import_id);

CREATE TABLE import_log (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    import_id   BIGINT NOT NULL REFERENCES imports(id) ON DELETE CASCADE,
    level       TEXT NOT NULL DEFAULT 'info' CHECK (level IN ('info', 'warn', 'error')),
    message     TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX import_log_import_idx ON import_log (import_id, id);

-- Conditional-request cache: a rerun answers 304 (free on GitHub's rate
-- limit) and reuses the stored page.
CREATE TABLE import_http_cache (
    import_id   BIGINT NOT NULL REFERENCES imports(id) ON DELETE CASCADE,
    url         TEXT NOT NULL,
    etag        TEXT NOT NULL,
    body        JSONB NOT NULL,
    link        TEXT,
    PRIMARY KEY (import_id, url)
);

-- Git step of a metadata import (P11 import without `mirror`): its Push
-- event carries origin `metadata-import`, which webhooks, notifications,
-- activity and commit-keyword closing skip (code search still indexes).
ALTER TABLE repo_imports ADD COLUMN quiet BOOLEAN NOT NULL DEFAULT false;
