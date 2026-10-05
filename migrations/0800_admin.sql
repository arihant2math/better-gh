-- Site administration (bgh-admin): impersonation tokens, storage quotas,
-- repository maintenance runs, site counters, audit/job indexes.

-- Impersonation tokens (`POST /admin/users/{u}/authorizations`) are access
-- tokens of kind 'impersonation' minted by a site admin.
ALTER TABLE access_tokens DROP CONSTRAINT IF EXISTS access_tokens_kind_check;
ALTER TABLE access_tokens ADD CONSTRAINT access_tokens_kind_check
    CHECK (kind IN ('pat', 'oauth', 'app', 'impersonation'));
ALTER TABLE access_tokens ADD COLUMN created_by_id BIGINT REFERENCES users (id) ON DELETE SET NULL;
CREATE INDEX access_tokens_impersonation_idx ON access_tokens (user_id) WHERE kind = 'impersonation';

-- Second factor state lives in `user_two_factor` / `user_recovery_codes`
-- (created by 0100_accounts.sql, owned by bgh-accounts); bgh-admin reads it
-- through `bgh_core::two_factor`.

-- Per-owner storage quotas (KB-based like repositories.size), set by site
-- admins. NULL limits fall back to site settings.
CREATE TABLE storage_quotas (
    owner_id            BIGINT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    -- Max size of a single repository in MB.
    max_repo_size_mb    BIGINT CHECK (max_repo_size_mb IS NULL OR max_repo_size_mb > 0),
    -- Max total size of all repositories of the owner in MB.
    max_total_size_mb   BIGINT CHECK (max_total_size_mb IS NULL OR max_total_size_mb > 0),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Repository maintenance requested by site admins (gc / fsck / repack /
-- size & language recalculation), executed by the `admin.repo_maintenance` job.
CREATE TABLE repo_maintenance_runs (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id         BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    operation       TEXT NOT NULL CHECK (operation IN (
                        'gc', 'fsck', 'repack', 'recalculate_size', 'recalculate_languages')),
    status          TEXT NOT NULL DEFAULT 'queued'
                    CHECK (status IN ('queued', 'running', 'succeeded', 'failed')),
    output          TEXT,
    requested_by_id BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at      TIMESTAMPTZ,
    finished_at     TIMESTAMPTZ
);
CREATE INDEX repo_maintenance_runs_repo_idx ON repo_maintenance_runs (repo_id, id DESC);

-- Monotonic site counters that can't be derived from tables (e.g. pushes).
CREATE TABLE site_counters (
    key         TEXT PRIMARY KEY,
    value       BIGINT NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Audit log search: cursor pagination on id within each filter.
CREATE INDEX audit_log_org_id_idx ON audit_log (org_id, id DESC) WHERE org_id IS NOT NULL;
CREATE INDEX audit_log_repo_id_idx ON audit_log (repo_id, id DESC) WHERE repo_id IS NOT NULL;
CREATE INDEX audit_log_actor_id_idx ON audit_log (actor_id, id DESC);
CREATE INDEX audit_log_action_prefix_idx ON audit_log (action text_pattern_ops, id DESC);
CREATE INDEX audit_log_target_idx ON audit_log (target_type, target_id, id DESC);

-- Job inspector: failed/kind listings and per-kind stats.
CREATE INDEX jobs_failed_idx ON jobs (failed_at DESC, id DESC) WHERE failed_at IS NOT NULL;
CREATE INDEX jobs_kind_id_idx ON jobs (kind, id DESC);

-- Admin user listing: filters on site_admin / suspended.
CREATE INDEX users_site_admin_idx ON users (id) WHERE site_admin;
CREATE INDEX users_suspended_idx ON users (suspended_at) WHERE suspended_at IS NOT NULL;
CREATE INDEX users_type_created_idx ON users (type, created_at DESC, id DESC);
CREATE INDEX repositories_size_idx ON repositories (size DESC, id);
CREATE INDEX repositories_owner_size_idx ON repositories (owner_id, size);
