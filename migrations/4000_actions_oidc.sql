-- P28: Actions OIDC id-tokens.

-- Subject (`sub`) claim templates (`/actions/oidc/customization/sub`):
-- one row per repository or organization. A repository row with
-- `use_default` takes the default template (`repo`, `context`); without it,
-- its own keys, or the organization's template when it lists none.
CREATE TABLE actions_oidc_sub_claims (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id             BIGINT REFERENCES repositories (id) ON DELETE CASCADE,
    org_id              BIGINT REFERENCES users (id) ON DELETE CASCADE,
    use_default         BOOLEAN NOT NULL DEFAULT true,
    include_claim_keys  TEXT[] NOT NULL DEFAULT '{}',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((repo_id IS NULL) <> (org_id IS NULL))
);
CREATE UNIQUE INDEX actions_oidc_sub_claims_repo_key
    ON actions_oidc_sub_claims (repo_id) WHERE repo_id IS NOT NULL;
CREATE UNIQUE INDEX actions_oidc_sub_claims_org_key
    ON actions_oidc_sub_claims (org_id) WHERE org_id IS NOT NULL;

-- The id-token request endpoint finds the running job by its GITHUB_TOKEN.
CREATE INDEX actions_jobs_token_idx ON actions_jobs (token_id) WHERE token_id IS NOT NULL;
