-- P47: fine-grained personal access tokens and organization token policies.

-- Fine-grained tokens are `access_tokens` rows of kind `fine_grained`. Their
-- resource owner, repository selection, approval state and permission map
-- (`permissions`: {repository, organization, account}) live on the row and
-- are mirrored into `scopes` (see bgh_core::pat) so authentication needs no
-- extra query.
ALTER TABLE access_tokens DROP CONSTRAINT IF EXISTS access_tokens_kind_check;
ALTER TABLE access_tokens ADD CONSTRAINT access_tokens_kind_check
    CHECK (kind IN ('pat', 'oauth', 'app', 'impersonation', 'fine_grained'));

ALTER TABLE access_tokens
    ADD COLUMN description          TEXT NOT NULL DEFAULT '',
    ADD COLUMN resource_owner_id    BIGINT REFERENCES users (id) ON DELETE CASCADE,
    ADD COLUMN repository_selection TEXT
        CHECK (repository_selection IN ('all', 'selected', 'public')),
    ADD COLUMN approval_status      TEXT
        CHECK (approval_status IN ('approved', 'pending', 'denied', 'revoked')),
    -- The requester's justification for the organization.
    ADD COLUMN approval_reason      TEXT,
    ADD COLUMN reviewed_by_id       BIGINT REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN reviewed_at          TIMESTAMPTZ,
    ADD COLUMN review_reason        TEXT;

CREATE INDEX access_tokens_resource_owner_idx
    ON access_tokens (resource_owner_id, approval_status, id)
    WHERE resource_owner_id IS NOT NULL;
CREATE INDEX access_tokens_reviewed_by_idx ON access_tokens (reviewed_by_id)
    WHERE reviewed_by_id IS NOT NULL;

-- Repositories selected for a fine-grained token (`repository_selection =
-- 'selected'`).
CREATE TABLE access_token_repos (
    token_id  BIGINT NOT NULL REFERENCES access_tokens (id) ON DELETE CASCADE,
    repo_id   BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    PRIMARY KEY (token_id, repo_id)
);
CREATE INDEX access_token_repos_repo_idx ON access_token_repos (repo_id);

-- Organization policy for personal access tokens. No row = defaults.
CREATE TABLE org_pat_policies (
    org_id                          BIGINT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    fine_grained_allowed            BOOLEAN NOT NULL DEFAULT true,
    fine_grained_require_approval   BOOLEAN NOT NULL DEFAULT false,
    fine_grained_max_lifetime_days  INTEGER CHECK (fine_grained_max_lifetime_days BETWEEN 1 AND 366),
    classic_allowed                 BOOLEAN NOT NULL DEFAULT true,
    classic_max_lifetime_days       INTEGER CHECK (classic_max_lifetime_days BETWEEN 1 AND 3650),
    updated_at                      TIMESTAMPTZ NOT NULL DEFAULT now()
);
