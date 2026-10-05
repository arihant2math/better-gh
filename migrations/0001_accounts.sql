-- Core schema: users, organizations, credentials, teams.
--
-- Conventions (see docs/BACKEND_PATTERNS.md):
--   * BIGINT GENERATED ALWAYS AS IDENTITY primary keys, TIMESTAMPTZ times.
--   * Repository-level permissions are stored as role names:
--     'read' | 'triage' | 'write' | 'maintain' | 'admin'.

-- Users and organizations share one table (GitHub-style `type`).
CREATE TABLE users (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    login             TEXT NOT NULL,
    type              TEXT NOT NULL DEFAULT 'User'
                      CHECK (type IN ('User', 'Organization', 'Bot')),
    name              TEXT,
    -- Public profile email (may differ from the primary email).
    email             TEXT,
    bio               TEXT,
    company           TEXT,
    location          TEXT,
    blog              TEXT,
    twitter_username  TEXT,
    hireable          BOOLEAN,
    -- NULL = default generated avatar.
    avatar_url        TEXT,
    site_admin        BOOLEAN NOT NULL DEFAULT false,
    suspended_at      TIMESTAMPTZ,
    suspended_reason  TEXT,
    -- Argon2id PHC string; NULL for organizations / bots / SSO-only users.
    password_hash     TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX users_login_key ON users (lower(login));
CREATE INDEX users_type_idx ON users (type);

CREATE TABLE user_emails (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    email       TEXT NOT NULL,
    verified    BOOLEAN NOT NULL DEFAULT false,
    is_primary  BOOLEAN NOT NULL DEFAULT false,
    visibility  TEXT CHECK (visibility IN ('public', 'private')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX user_emails_email_key ON user_emails (lower(email));
CREATE INDEX user_emails_user_idx ON user_emails (user_id);
CREATE UNIQUE INDEX user_emails_one_primary ON user_emails (user_id) WHERE is_primary;

CREATE TABLE follows (
    follower_id   BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    following_id  BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (follower_id, following_id)
);
CREATE INDEX follows_following_idx ON follows (following_id);

-- Browser sessions. The cookie carries a random token; only its SHA-256 is stored.
CREATE TABLE sessions (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    token_hash    TEXT NOT NULL UNIQUE,
    user_id       BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    user_agent    TEXT,
    ip            TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at    TIMESTAMPTZ NOT NULL
);
CREATE INDEX sessions_user_idx ON sessions (user_id);
CREATE INDEX sessions_expires_idx ON sessions (expires_at);

-- Personal access tokens (and later OAuth/app tokens via `kind`).
CREATE TABLE access_tokens (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id           BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind              TEXT NOT NULL DEFAULT 'pat' CHECK (kind IN ('pat', 'oauth', 'app')),
    name              TEXT NOT NULL DEFAULT '',
    token_hash        TEXT NOT NULL UNIQUE,
    token_last_eight  TEXT NOT NULL,
    scopes            TEXT[] NOT NULL DEFAULT '{}',
    expires_at        TIMESTAMPTZ,
    last_used_at      TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX access_tokens_user_idx ON access_tokens (user_id);

CREATE TABLE ssh_keys (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id       BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title         TEXT NOT NULL DEFAULT '',
    key           TEXT NOT NULL,
    -- "SHA256:<base64>" fingerprint, unique across users and deploy keys.
    fingerprint   TEXT NOT NULL UNIQUE,
    verified      BOOLEAN NOT NULL DEFAULT true,
    read_only     BOOLEAN NOT NULL DEFAULT false,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at  TIMESTAMPTZ
);
CREATE INDEX ssh_keys_user_idx ON ssh_keys (user_id);

CREATE TABLE gpg_keys (
    id                    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id               BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name                  TEXT,
    key_id                TEXT NOT NULL,
    primary_key_id        BIGINT REFERENCES gpg_keys (id) ON DELETE CASCADE,
    public_key            TEXT NOT NULL,
    raw_key               TEXT,
    emails                JSONB NOT NULL DEFAULT '[]',
    can_sign              BOOLEAN NOT NULL DEFAULT true,
    can_encrypt_comms     BOOLEAN NOT NULL DEFAULT false,
    can_encrypt_storage   BOOLEAN NOT NULL DEFAULT false,
    can_certify           BOOLEAN NOT NULL DEFAULT false,
    expires_at            TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX gpg_keys_user_idx ON gpg_keys (user_id);
CREATE INDEX gpg_keys_key_id_idx ON gpg_keys (key_id);

-- Organization-only settings (1:1 with users rows of type 'Organization').
CREATE TABLE org_settings (
    org_id                               BIGINT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    description                          TEXT,
    billing_email                        TEXT,
    is_verified                          BOOLEAN NOT NULL DEFAULT false,
    -- Base permission for members on all org repos: 'none' or a role name.
    default_repository_permission        TEXT NOT NULL DEFAULT 'read'
        CHECK (default_repository_permission IN ('none', 'read', 'write', 'admin')),
    members_can_create_repositories      BOOLEAN NOT NULL DEFAULT true,
    members_can_create_public_repositories  BOOLEAN NOT NULL DEFAULT true,
    members_can_create_private_repositories BOOLEAN NOT NULL DEFAULT true,
    members_can_fork_private_repositories   BOOLEAN NOT NULL DEFAULT false,
    two_factor_requirement_enabled       BOOLEAN NOT NULL DEFAULT false,
    has_organization_projects            BOOLEAN NOT NULL DEFAULT true,
    has_repository_projects              BOOLEAN NOT NULL DEFAULT true,
    archived_at                          TIMESTAMPTZ
);

CREATE TABLE org_members (
    org_id      BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role        TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('admin', 'member')),
    -- Publicized membership (GET /orgs/{org}/public_members).
    is_public   BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, user_id)
);
CREATE INDEX org_members_user_idx ON org_members (user_id);

CREATE TABLE org_invitations (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    org_id        BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    invitee_id    BIGINT REFERENCES users (id) ON DELETE CASCADE,
    email         TEXT,
    inviter_id    BIGINT REFERENCES users (id) ON DELETE SET NULL,
    role          TEXT NOT NULL DEFAULT 'direct_member'
                  CHECK (role IN ('admin', 'direct_member', 'billing_manager')),
    team_ids      BIGINT[] NOT NULL DEFAULT '{}',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    failed_at     TIMESTAMPTZ,
    CHECK (invitee_id IS NOT NULL OR email IS NOT NULL)
);
CREATE INDEX org_invitations_org_idx ON org_invitations (org_id);
CREATE INDEX org_invitations_invitee_idx ON org_invitations (invitee_id);

CREATE TABLE teams (
    id                    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    org_id                BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    parent_id             BIGINT REFERENCES teams (id) ON DELETE SET NULL,
    name                  TEXT NOT NULL,
    slug                  TEXT NOT NULL,
    description           TEXT,
    privacy               TEXT NOT NULL DEFAULT 'closed' CHECK (privacy IN ('secret', 'closed')),
    notification_setting  TEXT NOT NULL DEFAULT 'notifications_enabled'
                          CHECK (notification_setting IN ('notifications_enabled', 'notifications_disabled')),
    -- Default permission when adding repos to the team (legacy field).
    permission            TEXT NOT NULL DEFAULT 'read'
                          CHECK (permission IN ('read', 'triage', 'write', 'maintain', 'admin')),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX teams_org_slug_key ON teams (org_id, lower(slug));
CREATE UNIQUE INDEX teams_org_name_key ON teams (org_id, lower(name));
CREATE INDEX teams_parent_idx ON teams (parent_id);

CREATE TABLE team_members (
    team_id     BIGINT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role        TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('member', 'maintainer')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (team_id, user_id)
);
CREATE INDEX team_members_user_idx ON team_members (user_id);
