-- P49: SAML single sign-on and SCIM provisioning.
-- SAML-linked accounts are `user_identities` rows with provider 'saml' and
-- the NameID as subject (bgh_accounts::saml).

-- Keys imported from SAML attributes (replaced at every sign-in; keys users
-- added themselves are never touched).
ALTER TABLE ssh_keys ADD COLUMN saml_synced BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE gpg_keys ADD COLUMN saml_synced BOOLEAN NOT NULL DEFAULT false;

-- SCIM users (bgh_accounts::scim). `org_id` NULL = provisioned by the
-- enterprise (instance-wide) endpoints, else by the organization's.
CREATE TABLE scim_users (
    id                 UUID PRIMARY KEY,
    org_id             BIGINT REFERENCES users (id) ON DELETE CASCADE,
    user_id            BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    external_id        TEXT,
    user_name          TEXT NOT NULL,
    display_name       TEXT,
    given_name         TEXT,
    family_name        TEXT,
    formatted_name     TEXT,
    emails             JSONB NOT NULL DEFAULT '[]',
    roles              JSONB NOT NULL DEFAULT '[]',
    active             BOOLEAN NOT NULL DEFAULT true,
    -- The account was suspended by SCIM deprovisioning (lifted only when
    -- the IdP reactivates it; manual suspensions are never lifted).
    suspended_by_scim  BOOLEAN NOT NULL DEFAULT false,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX scim_users_user_name_key ON scim_users (coalesce(org_id, 0), lower(user_name));
CREATE UNIQUE INDEX scim_users_user_key ON scim_users (coalesce(org_id, 0), user_id);
CREATE INDEX scim_users_external_idx ON scim_users (coalesce(org_id, 0), external_id);
CREATE INDEX scim_users_user_idx ON scim_users (user_id);

-- SCIM groups (enterprise endpoints); teams follow them through
-- `external_group_mappings` (provider 'scim', matched by display name,
-- id or externalId).
CREATE TABLE scim_groups (
    id            UUID PRIMARY KEY,
    external_id   TEXT,
    display_name  TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX scim_groups_display_name_key ON scim_groups (lower(display_name));
CREATE INDEX scim_groups_external_idx ON scim_groups (external_id);

CREATE TABLE scim_group_members (
    group_id      UUID NOT NULL REFERENCES scim_groups (id) ON DELETE CASCADE,
    scim_user_id  UUID NOT NULL REFERENCES scim_users (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, scim_user_id)
);
CREATE INDEX scim_group_members_user_idx ON scim_group_members (scim_user_id);
