-- P65: secret scanning and push protection (bgh-security).

-- Per-repository `security_and_analysis` toggles. A missing row means
-- every feature is disabled (the site setting `secret_scanning.enable_all`
-- / `push_protection_all` can still force them on).
CREATE TABLE repo_security_settings (
    repo_id                 BIGINT PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    secret_scanning         BOOLEAN NOT NULL DEFAULT false,
    push_protection         BOOLEAN NOT NULL DEFAULT false,
    non_provider_patterns   BOOLEAN NOT NULL DEFAULT false,
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Custom patterns of a repository or an organization (exactly one owner).
CREATE TABLE secret_scanning_custom_patterns (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id         BIGINT REFERENCES repositories (id) ON DELETE CASCADE,
    org_id          BIGINT REFERENCES users (id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    pattern         TEXT NOT NULL,
    test_string     TEXT,
    push_protection BOOLEAN NOT NULL DEFAULT false,
    created_by_id   BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((repo_id IS NULL) <> (org_id IS NULL))
);
CREATE INDEX secret_scanning_custom_patterns_repo_idx
    ON secret_scanning_custom_patterns (repo_id, id) WHERE repo_id IS NOT NULL;
CREATE INDEX secret_scanning_custom_patterns_org_idx
    ON secret_scanning_custom_patterns (org_id, id) WHERE org_id IS NOT NULL;
CREATE INDEX secret_scanning_custom_patterns_creator_idx
    ON secret_scanning_custom_patterns (created_by_id);

-- Alerts, deduplicated per repository by (secret type, SHA-256 of the secret).
CREATE TABLE secret_scanning_alerts (
    id                              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id                         BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    number                          BIGINT NOT NULL,
    secret_type                     TEXT NOT NULL,
    secret_type_display_name        TEXT NOT NULL,
    secret_hash                     TEXT NOT NULL,
    -- `bgh_core::secretbox` sealed secret.
    secret_sealed                   BYTEA NOT NULL,
    custom_pattern_id               BIGINT REFERENCES secret_scanning_custom_patterns (id) ON DELETE SET NULL,
    state                           TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'resolved')),
    resolution                      TEXT CHECK (resolution IN ('false_positive', 'wont_fix', 'revoked', 'used_in_tests', 'pattern_deleted', 'pattern_edited')),
    resolution_comment              TEXT,
    resolved_by_id                  BIGINT REFERENCES users (id) ON DELETE SET NULL,
    resolved_at                     TIMESTAMPTZ,
    push_protection_bypassed        BOOLEAN NOT NULL DEFAULT false,
    push_protection_bypassed_by_id  BIGINT REFERENCES users (id) ON DELETE SET NULL,
    push_protection_bypassed_at     TIMESTAMPTZ,
    created_at                      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, number),
    UNIQUE (repo_id, secret_type, secret_hash)
);
CREATE INDEX secret_scanning_alerts_list_idx ON secret_scanning_alerts (repo_id, state, created_at DESC, id);
CREATE INDEX secret_scanning_alerts_resolver_idx ON secret_scanning_alerts (resolved_by_id);
CREATE INDEX secret_scanning_alerts_bypasser_idx ON secret_scanning_alerts (push_protection_bypassed_by_id);
CREATE INDEX secret_scanning_alerts_pattern_idx ON secret_scanning_alerts (custom_pattern_id);

-- Where a secret was found (GitHub's `commit` location type).
CREATE TABLE secret_scanning_locations (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    alert_id        BIGINT NOT NULL REFERENCES secret_scanning_alerts (id) ON DELETE CASCADE,
    commit_sha      TEXT NOT NULL,
    path            TEXT NOT NULL,
    blob_sha        TEXT NOT NULL,
    start_line      INTEGER NOT NULL,
    end_line        INTEGER NOT NULL,
    start_column    INTEGER NOT NULL,
    end_column      INTEGER NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (alert_id, commit_sha, path, start_line, start_column)
);

-- Secrets a push was blocked for. The pusher (or an admin) bypasses one
-- through the unblock URL; a bypassed secret may then be pushed by that
-- user until `expires_at`.
CREATE TABLE secret_scanning_push_blocks (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    placeholder_id      TEXT NOT NULL UNIQUE,
    repo_id             BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    user_id             BIGINT REFERENCES users (id) ON DELETE CASCADE,
    secret_type         TEXT NOT NULL,
    secret_type_display_name TEXT NOT NULL,
    secret_hash         TEXT NOT NULL,
    -- First characters of the secret only (shown on the unblock page).
    secret_preview      TEXT NOT NULL,
    commit_sha          TEXT NOT NULL,
    path                TEXT NOT NULL,
    start_line          INTEGER NOT NULL,
    reason              TEXT CHECK (reason IN ('false_positive', 'used_in_tests', 'will_fix_later')),
    bypassed_at         TIMESTAMPTZ,
    expires_at          TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX secret_scanning_push_blocks_secret_idx
    ON secret_scanning_push_blocks (repo_id, secret_hash, user_id) WHERE bypassed_at IS NOT NULL;
CREATE INDEX secret_scanning_push_blocks_user_idx ON secret_scanning_push_blocks (user_id);
CREATE INDEX secret_scanning_push_blocks_created_idx ON secret_scanning_push_blocks (created_at);

-- Scan history (`GET /repos/{o}/{r}/secret-scanning/scan-history`).
CREATE TABLE secret_scanning_scans (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id         BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('incremental', 'backfill', 'custom_pattern_backfill', 'pattern_update')),
    status          TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'completed')),
    blobs_scanned   BIGINT NOT NULL DEFAULT 0,
    bytes_scanned   BIGINT NOT NULL DEFAULT 0,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at    TIMESTAMPTZ
);
CREATE INDEX secret_scanning_scans_repo_idx ON secret_scanning_scans (repo_id, kind, started_at DESC);
