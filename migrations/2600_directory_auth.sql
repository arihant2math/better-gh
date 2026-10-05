-- P14: directory (LDAP / IdP group) team sync.
-- A team mapped to external groups has its membership managed by the
-- provider: `ldap` (external_group_id = normalized group DN) or
-- `oidc:{provider name}` (external_group_id = a value of the groups claim).
-- Reused by SAML/SCIM group sync (P49).
CREATE TABLE external_group_mappings (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    provider           TEXT NOT NULL,
    external_group_id  TEXT NOT NULL,
    team_id            BIGINT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    synced_at          TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, external_group_id, team_id)
);
CREATE INDEX external_group_mappings_team_idx ON external_group_mappings (team_id);

-- LDAP-linked accounts are `user_identities` rows with provider 'ldap' and
-- the normalized entry DN as subject; sync bookkeeping lives here.
CREATE TABLE ldap_user_sync (
    user_id      BIGINT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    synced_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The account was suspended by LDAP sync (lifted when the entry is
    -- active again); manual suspensions are never lifted by sync.
    suspended_by_sync BOOLEAN NOT NULL DEFAULT false
);

-- Keys imported from the directory (replaced on every sync; keys users
-- added themselves are never touched).
ALTER TABLE ssh_keys ADD COLUMN ldap_synced BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE gpg_keys ADD COLUMN ldap_synced BOOLEAN NOT NULL DEFAULT false;
