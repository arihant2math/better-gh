-- P17: GitHub Apps (registration, private keys, installations, installation
-- tokens). See docs/packages/p17-github-apps.md.

CREATE TABLE github_apps (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- User or organization owning the registration.
    owner_id            BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- `{slug}[bot]` (type Bot): author of everything done with the app's
    -- installation tokens. Deleting the bot deletes the app.
    bot_user_id         BIGINT NOT NULL UNIQUE REFERENCES users (id) ON DELETE CASCADE,
    slug                TEXT NOT NULL,
    name                TEXT NOT NULL,
    description         TEXT NOT NULL DEFAULT '',
    homepage_url        TEXT NOT NULL,
    callback_urls       TEXT[] NOT NULL DEFAULT '{}',
    setup_url           TEXT,
    setup_on_update     BOOLEAN NOT NULL DEFAULT false,
    -- Webhook configuration (delivery: P46). The secret is sealed with
    -- bgh_core::secretbox.
    webhook_active      BOOLEAN NOT NULL DEFAULT false,
    webhook_url         TEXT,
    webhook_secret      BYTEA,
    -- `{"contents": "read", "issues": "write", ...}` (GitHub's names).
    permissions         JSONB NOT NULL DEFAULT '{}',
    events              TEXT[] NOT NULL DEFAULT '{}',
    -- Public apps can be installed by anyone, private ones only on the
    -- owning account.
    public              BOOLEAN NOT NULL DEFAULT false,
    client_id           TEXT NOT NULL UNIQUE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX github_apps_slug_key ON github_apps (lower(slug));
CREATE UNIQUE INDEX github_apps_name_key ON github_apps (lower(name));
CREATE INDEX github_apps_owner_idx ON github_apps (owner_id, id);

-- RSA private keys: only the public half is kept (the PEM is shown once).
CREATE TABLE github_app_keys (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    app_id          BIGINT NOT NULL REFERENCES github_apps (id) ON DELETE CASCADE,
    -- SubjectPublicKeyInfo PEM.
    public_key      TEXT NOT NULL,
    -- `SHA256:<base64>` of the DER public key (GitHub's key fingerprint).
    fingerprint     TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX github_app_keys_app_idx ON github_app_keys (app_id, id);

CREATE TABLE app_installations (
    id                    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    app_id                BIGINT NOT NULL REFERENCES github_apps (id) ON DELETE CASCADE,
    -- User or organization the app is installed on.
    account_id            BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    repository_selection  TEXT NOT NULL CHECK (repository_selection IN ('all', 'selected')),
    -- Permissions and events accepted at install time.
    permissions           JSONB NOT NULL DEFAULT '{}',
    events                TEXT[] NOT NULL DEFAULT '{}',
    installed_by_id       BIGINT REFERENCES users (id) ON DELETE SET NULL,
    suspended_at          TIMESTAMPTZ,
    suspended_by_id       BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (app_id, account_id)
);
CREATE INDEX app_installations_account_idx ON app_installations (account_id, id);
CREATE INDEX app_installations_app_idx ON app_installations (app_id, id);

-- Selected repositories of `repository_selection = 'selected'` installations.
CREATE TABLE app_installation_repos (
    installation_id  BIGINT NOT NULL REFERENCES app_installations (id) ON DELETE CASCADE,
    repo_id          BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (installation_id, repo_id)
);
CREATE INDEX app_installation_repos_repo_idx ON app_installation_repos (repo_id);

-- Installation access tokens are `access_tokens` rows of kind `app` owned by
-- the app's bot user.
ALTER TABLE access_tokens
    ADD COLUMN installation_id BIGINT REFERENCES app_installations (id) ON DELETE CASCADE;
CREATE INDEX access_tokens_installation_idx ON access_tokens (installation_id)
    WHERE installation_id IS NOT NULL;
