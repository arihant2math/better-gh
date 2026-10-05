-- P36 account security: WebAuthn credentials, sudo mode, encrypted TOTP
-- secrets, org 2FA requirement removals, PAT expiry reminders.

-- TOTP secrets are stored encrypted with the server key
-- (`bgh_core::secretbox`); the plaintext column is legacy and is emptied by
-- `bgh_accounts::security::encrypt_legacy_totp` (run at start-up and on
-- first use of a row).
ALTER TABLE user_two_factor ADD COLUMN totp_secret_enc BYTEA;
ALTER TABLE user_two_factor ALTER COLUMN totp_secret DROP NOT NULL;
ALTER TABLE user_two_factor ADD CONSTRAINT user_two_factor_secret_present
    CHECK (totp_secret IS NOT NULL OR totp_secret_enc IS NOT NULL);
CREATE INDEX user_two_factor_legacy_idx ON user_two_factor (user_id) WHERE totp_secret IS NOT NULL;

-- Sudo mode: the last re-authentication of a browser session. New sessions
-- start in sudo mode (signing in is a fresh authentication); sessions that
-- existed before this migration have none.
ALTER TABLE sessions ADD COLUMN sudo_at TIMESTAMPTZ;
ALTER TABLE sessions ALTER COLUMN sudo_at SET DEFAULT now();

-- WebAuthn: one random user handle per account (sent to authenticators,
-- returned by discoverable credentials).
CREATE TABLE user_webauthn_handles (
    user_id     BIGINT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    handle      UUID NOT NULL UNIQUE
);

-- Registered security keys (second factor) and passkeys (passwordless,
-- discoverable). `credential` is webauthn-rs' serialized `Credential`.
CREATE TABLE user_webauthn_credentials (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id         BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('security_key', 'passkey')),
    name            TEXT NOT NULL,
    credential_id   BYTEA NOT NULL UNIQUE,
    credential      JSONB NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at    TIMESTAMPTZ
);
CREATE INDEX user_webauthn_credentials_user_idx ON user_webauthn_credentials (user_id, id);

-- Members and outside collaborators removed when an organization started
-- requiring 2FA, kept for reinstatement when they rejoin.
CREATE TABLE org_two_factor_removals (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    org_id          BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    user_id         BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('member', 'outside_collaborator')),
    -- Org role ('admin' | 'member') for members.
    role            TEXT,
    team_ids        BIGINT[] NOT NULL DEFAULT '{}',
    -- Direct repository grants: [{"repo_id": 1, "permission": "write"}].
    repositories    JSONB NOT NULL DEFAULT '[]',
    removed_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    reinstated_at   TIMESTAMPTZ,
    UNIQUE (org_id, user_id)
);
CREATE INDEX org_two_factor_removals_user_idx ON org_two_factor_removals (user_id);

-- PAT expiry reminders already sent (7 days and 1 day before expiry).
ALTER TABLE access_tokens ADD COLUMN expiry_notified_7d_at TIMESTAMPTZ;
ALTER TABLE access_tokens ADD COLUMN expiry_notified_1d_at TIMESTAMPTZ;
CREATE INDEX access_tokens_expiring_idx ON access_tokens (expires_at)
    WHERE expires_at IS NOT NULL AND kind = 'pat';
