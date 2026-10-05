-- Core schema: webhooks, notifications, releases.

-- Repository hooks (repo_id set), organization hooks (org_id set) or
-- site-wide/global hooks (both NULL).
CREATE TABLE webhooks (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id        BIGINT REFERENCES repositories (id) ON DELETE CASCADE,
    org_id         BIGINT REFERENCES users (id) ON DELETE CASCADE,
    name           TEXT NOT NULL DEFAULT 'web',
    url            TEXT NOT NULL,
    content_type   TEXT NOT NULL DEFAULT 'form' CHECK (content_type IN ('json', 'form')),
    secret         TEXT,
    insecure_ssl   BOOLEAN NOT NULL DEFAULT false,
    events         TEXT[] NOT NULL DEFAULT '{push}',
    active         BOOLEAN NOT NULL DEFAULT true,
    -- {"code": int|null, "status": "active"|"unused"|..., "message": str|null}
    last_response  JSONB NOT NULL DEFAULT '{"code": null, "status": "unused", "message": null}',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (repo_id IS NULL OR org_id IS NULL)
);
CREATE INDEX webhooks_repo_idx ON webhooks (repo_id) WHERE repo_id IS NOT NULL;
CREATE INDEX webhooks_org_idx ON webhooks (org_id) WHERE org_id IS NOT NULL;

CREATE TABLE webhook_deliveries (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    hook_id           BIGINT NOT NULL REFERENCES webhooks (id) ON DELETE CASCADE,
    guid              UUID NOT NULL,
    event             TEXT NOT NULL,
    action            TEXT,
    repo_id           BIGINT,
    installation_id   BIGINT,
    redelivery        BOOLEAN NOT NULL DEFAULT false,
    status            TEXT NOT NULL DEFAULT 'pending',
    status_code       INTEGER,
    duration_ms       INTEGER,
    request_headers   JSONB NOT NULL DEFAULT '{}',
    request_payload   JSONB NOT NULL DEFAULT '{}',
    response_headers  JSONB,
    response_body     TEXT,
    delivered_at      TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX webhook_deliveries_hook_idx ON webhook_deliveries (hook_id, id DESC);
CREATE INDEX webhook_deliveries_created_idx ON webhook_deliveries (created_at);

-- Notification threads: one row per (user, subject).
CREATE TABLE notifications (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id       BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    repo_id       BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    -- 'Issue' | 'PullRequest' | 'Release' | 'Commit' | 'Discussion' | 'CheckSuite' ...
    subject_type  TEXT NOT NULL,
    subject_id    BIGINT NOT NULL,
    subject_title TEXT NOT NULL DEFAULT '',
    -- GitHub reasons: assign, author, comment, invitation, manual, mention,
    -- review_requested, security_alert, state_change, subscribed, team_mention, ci_activity
    reason        TEXT NOT NULL,
    unread        BOOLEAN NOT NULL DEFAULT true,
    done          BOOLEAN NOT NULL DEFAULT false,
    last_read_at  TIMESTAMPTZ,
    -- Latest comment/event that bumped the thread (for latest_comment_url).
    latest_comment_id BIGINT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, subject_type, subject_id)
);
CREATE INDEX notifications_user_updated_idx ON notifications (user_id, updated_at DESC) WHERE NOT done;
CREATE INDEX notifications_user_repo_idx ON notifications (user_id, repo_id, updated_at DESC);

-- Per-thread subscription overrides (subscribe/unsubscribe/ignore a thread).
CREATE TABLE thread_subscriptions (
    user_id       BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    subject_type  TEXT NOT NULL,
    subject_id    BIGINT NOT NULL,
    repo_id       BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    subscribed    BOOLEAN NOT NULL DEFAULT true,
    ignored       BOOLEAN NOT NULL DEFAULT false,
    reason        TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, subject_type, subject_id)
);
CREATE INDEX thread_subscriptions_subject_idx ON thread_subscriptions (subject_type, subject_id);

CREATE TABLE releases (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id           BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    tag_name          TEXT NOT NULL,
    target_commitish  TEXT NOT NULL,
    name              TEXT,
    body              TEXT,
    draft             BOOLEAN NOT NULL DEFAULT false,
    prerelease        BOOLEAN NOT NULL DEFAULT false,
    -- Explicit "latest" marker; NULL means computed by date/semver.
    make_latest       BOOLEAN,
    author_id         BIGINT REFERENCES users (id) ON DELETE SET NULL,
    published_at      TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX releases_repo_tag_published_key ON releases (repo_id, tag_name) WHERE NOT draft;
CREATE INDEX releases_repo_idx ON releases (repo_id, created_at DESC);

CREATE TABLE release_assets (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    release_id      BIGINT NOT NULL REFERENCES releases (id) ON DELETE CASCADE,
    repo_id         BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    label           TEXT,
    content_type    TEXT NOT NULL DEFAULT 'application/octet-stream',
    size            BIGINT NOT NULL DEFAULT 0,
    -- Hex SHA-256 of the content; storage path is derived from it.
    sha256          TEXT,
    state           TEXT NOT NULL DEFAULT 'uploaded' CHECK (state IN ('uploaded', 'open')),
    download_count  BIGINT NOT NULL DEFAULT 0,
    uploader_id     BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (release_id, name)
);
