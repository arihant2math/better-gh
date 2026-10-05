-- Accounts package (bgh-accounts): 2FA, account tokens, OAuth apps, SSO
-- identities, avatars, blocks; extensions of core account tables.

-- Stable row id for org memberships (sync model `membership`).
ALTER TABLE org_members ADD COLUMN id BIGINT GENERATED ALWAYS AS IDENTITY;
CREATE UNIQUE INDEX org_members_id_key ON org_members (id);
CREATE INDEX org_members_org_role_idx ON org_members (org_id, role, user_id);

ALTER TABLE org_settings
    ADD COLUMN members_can_create_teams BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN web_commit_signoff_required BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE org_invitations ADD COLUMN failed_reason TEXT;
CREATE UNIQUE INDEX org_invitations_pending_user_key
    ON org_invitations (org_id, invitee_id) WHERE invitee_id IS NOT NULL AND failed_at IS NULL;
CREATE UNIQUE INDEX org_invitations_pending_email_key
    ON org_invitations (org_id, lower(email)) WHERE email IS NOT NULL AND failed_at IS NULL;
CREATE INDEX org_invitations_email_idx ON org_invitations (lower(email)) WHERE email IS NOT NULL;

CREATE INDEX teams_org_idx ON teams (org_id, lower(name));
CREATE INDEX follows_follower_idx ON follows (follower_id, created_at);
CREATE INDEX users_id_type_idx ON users (id) WHERE type IN ('User', 'Organization');

-- One-time secrets mailed to users (only the SHA-256 is stored).
CREATE TABLE account_tokens (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('password_reset', 'email_verification')),
    token_hash  TEXT NOT NULL UNIQUE,
    email_id    BIGINT REFERENCES user_emails (id) ON DELETE CASCADE,
    expires_at  TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX account_tokens_user_idx ON account_tokens (user_id, kind);
CREATE INDEX account_tokens_expires_idx ON account_tokens (expires_at);

-- TOTP two-factor authentication. `enabled_at IS NULL` = setup pending.
CREATE TABLE user_two_factor (
    user_id         BIGINT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    -- RFC 4648 base32 secret.
    totp_secret     TEXT NOT NULL,
    enabled_at      TIMESTAMPTZ,
    -- Last accepted TOTP time step (replay protection).
    last_used_step  BIGINT NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_recovery_codes (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    code_hash   TEXT NOT NULL,
    used_at     TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX user_recovery_codes_user_idx ON user_recovery_codes (user_id);

-- Users (or organizations) blocking users.
CREATE TABLE user_blocks (
    blocker_id  BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    blocked_id  BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (blocker_id, blocked_id)
);
CREATE INDEX user_blocks_blocked_idx ON user_blocks (blocked_id);

-- OAuth applications. `owner_id IS NULL` = built-in (e.g. the gh CLI).
CREATE TABLE oauth_apps (
    id                        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner_id                  BIGINT REFERENCES users (id) ON DELETE CASCADE,
    name                      TEXT NOT NULL,
    description               TEXT,
    homepage_url              TEXT NOT NULL DEFAULT '',
    callback_url              TEXT NOT NULL DEFAULT '',
    client_id                 TEXT NOT NULL UNIQUE,
    -- SHA-256 of the client secret; NULL = public client (no secret).
    client_secret_hash        TEXT,
    client_secret_last_eight  TEXT,
    device_flow_enabled       BOOLEAN NOT NULL DEFAULT false,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX oauth_apps_owner_idx ON oauth_apps (owner_id);

-- The GitHub CLI's public client id, so `gh auth login --hostname ...`
-- works out of the box (device flow, no secret check).
INSERT INTO oauth_apps (name, description, homepage_url, callback_url, client_id, device_flow_enabled)
VALUES ('GitHub CLI', 'Built-in application for the gh command line tool',
        'https://cli.github.com/', 'http://127.0.0.1/callback', '178c6fc778ccc68e1d6a', true);

-- A user's grant to an OAuth app (remembered consent, revocable).
CREATE TABLE oauth_authorizations (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    app_id      BIGINT NOT NULL REFERENCES oauth_apps (id) ON DELETE CASCADE,
    scopes      TEXT[] NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, app_id)
);
CREATE INDEX oauth_authorizations_app_idx ON oauth_authorizations (app_id);

ALTER TABLE access_tokens ADD COLUMN oauth_app_id BIGINT REFERENCES oauth_apps (id) ON DELETE CASCADE;
CREATE INDEX access_tokens_oauth_app_idx ON access_tokens (oauth_app_id, user_id) WHERE oauth_app_id IS NOT NULL;

-- External identities (OIDC SSO) linked to local accounts.
CREATE TABLE user_identities (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id        BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    provider       TEXT NOT NULL,
    subject        TEXT NOT NULL,
    email          TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_login_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, subject)
);
CREATE INDEX user_identities_user_idx ON user_identities (user_id);

-- Uploaded avatars (users and organizations), served at /avatars/u/{id}.
CREATE TABLE user_avatars (
    user_id       BIGINT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    content_type  TEXT NOT NULL,
    sha256        TEXT NOT NULL,
    data          BYTEA NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
