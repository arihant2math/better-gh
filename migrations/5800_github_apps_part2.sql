-- P46: GitHub Apps, part 2 (app webhooks, installation events, manifest
-- flow, user-to-server tokens, checks attribution). See
-- docs/packages/p46-github-apps-2.md.

-- App webhook configuration (`/app/hook/config`); URL, secret and active
-- flag are P17's `webhook_*` columns.
ALTER TABLE github_apps
    ADD COLUMN webhook_content_type TEXT NOT NULL DEFAULT 'json'
        CHECK (webhook_content_type IN ('json', 'form')),
    ADD COLUMN webhook_insecure_ssl BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN webhook_last_response JSONB NOT NULL
        DEFAULT '{"code": null, "status": "unused", "message": null}';

-- App hook deliveries share the delivery log with repository, organization
-- and global hooks: exactly one of `hook_id` / `app_id` is set.
ALTER TABLE webhook_deliveries
    ALTER COLUMN hook_id DROP NOT NULL,
    ADD COLUMN app_id BIGINT REFERENCES github_apps (id) ON DELETE CASCADE,
    ADD CONSTRAINT webhook_deliveries_target_check
        CHECK (hook_id IS NOT NULL OR app_id IS NOT NULL) NOT VALID;
CREATE INDEX webhook_deliveries_app_idx ON webhook_deliveries (app_id, id DESC)
    WHERE app_id IS NOT NULL;
-- One delivery per (app, outbox event, payload) like hooks.
CREATE UNIQUE INDEX webhook_deliveries_app_event_idx
    ON webhook_deliveries (app_id, event_id, event_seq)
    WHERE app_id IS NOT NULL AND event_id IS NOT NULL;

-- Client secrets of an app (OAuth for user-to-server tokens, manifest
-- conversion). Only a SHA-256 is kept.
CREATE TABLE github_app_client_secrets (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    app_id        BIGINT NOT NULL REFERENCES github_apps (id) ON DELETE CASCADE,
    secret_hash   TEXT NOT NULL,
    last_eight    TEXT NOT NULL,
    creator_id    BIGINT REFERENCES users (id) ON DELETE SET NULL,
    last_used_at  TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX github_app_client_secrets_app_idx ON github_app_client_secrets (app_id, id);

-- Manifest flow: a posted manifest awaiting the user's confirmation
-- (`pending`), then the one-time conversion `code` handed to the
-- manifest's redirect URL. Credentials are sealed with bgh_core::secretbox
-- until converted (1 hour), then wiped.
CREATE TABLE github_app_manifests (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Random token naming the pending manifest in the confirmation URL.
    token            TEXT NOT NULL UNIQUE,
    -- Who confirmed it (set when the app is created).
    user_id          BIGINT REFERENCES users (id) ON DELETE CASCADE,
    -- Organization the app will belong to (NULL: the confirming user).
    org_id           BIGINT REFERENCES users (id) ON DELETE CASCADE,
    manifest         JSONB NOT NULL,
    state            TEXT,
    app_id           BIGINT REFERENCES github_apps (id) ON DELETE CASCADE,
    -- SHA-256 of the conversion code, set when the app is created.
    code_hash        TEXT UNIQUE,
    credentials      BYTEA,
    converted_at     TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX github_app_manifests_created_idx ON github_app_manifests (created_at);

-- User-to-server tokens (`bghu_…`, kind `app`, owned by the user) record
-- their app; refresh tokens (`bghr_…`) are single use.
ALTER TABLE access_tokens
    ADD COLUMN github_app_id BIGINT REFERENCES github_apps (id) ON DELETE CASCADE;
CREATE INDEX access_tokens_github_app_idx ON access_tokens (github_app_id, user_id)
    WHERE github_app_id IS NOT NULL;

CREATE TABLE github_app_refresh_tokens (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    app_id        BIGINT NOT NULL REFERENCES github_apps (id) ON DELETE CASCADE,
    user_id       BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash    TEXT NOT NULL UNIQUE,
    expires_at    TIMESTAMPTZ NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX github_app_refresh_tokens_user_idx ON github_app_refresh_tokens (app_id, user_id);

-- Users who authorized an app (user-to-server); revoking deletes tokens.
CREATE TABLE github_app_authorizations (
    app_id      BIGINT NOT NULL REFERENCES github_apps (id) ON DELETE CASCADE,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (app_id, user_id)
);
CREATE INDEX github_app_authorizations_user_idx ON github_app_authorizations (user_id);

-- Check suites belong to a real app when created with its credentials;
-- NULL for the built-in Actions app and REST calls by users.
ALTER TABLE check_suites
    ADD COLUMN app_id BIGINT REFERENCES github_apps (id) ON DELETE SET NULL;
CREATE INDEX check_suites_app_idx ON check_suites (app_id) WHERE app_id IS NOT NULL;

-- The built-in Actions app has GitHub's id 15368; real app ids stay above
-- it so required-check sources (`app_id`) are unambiguous.
SELECT setval(pg_get_serial_sequence('github_apps', 'id'),
              GREATEST((SELECT max(id) FROM github_apps), 15368));

-- Branch protection checks that named the old synthetic ids: 1 (Actions)
-- becomes 15368, 2 (REST API) any source.
UPDATE branch_protections
   SET required_status_checks = jsonb_set(required_status_checks, '{checks}', (
       SELECT coalesce(jsonb_agg(CASE
                  WHEN c->'app_id' = '1'::jsonb THEN jsonb_set(c, '{app_id}', '15368'::jsonb)
                  WHEN c->'app_id' = '2'::jsonb THEN jsonb_set(c, '{app_id}', 'null'::jsonb)
                  ELSE c END), '[]'::jsonb)
         FROM jsonb_array_elements(required_status_checks->'checks') c))
 WHERE jsonb_typeof(required_status_checks->'checks') = 'array';
