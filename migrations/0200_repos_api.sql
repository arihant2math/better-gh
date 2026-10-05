-- bgh-repos: REST API support tables (redirects, languages, rulesets,
-- autolinks) and indexes for list endpoints.

-- Old `owner/name` of renamed or transferred repositories. Lookups that miss
-- `repositories` fall back here (see bgh_core::perms::RepoAccess::load).
-- Creating a repository with the old name removes the redirect.
CREATE TABLE repo_redirects (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner_login  TEXT NOT NULL,
    name         TEXT NOT NULL,
    repo_id      BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX repo_redirects_name_key ON repo_redirects (lower(owner_login), lower(name));
CREATE INDEX repo_redirects_repo_idx ON repo_redirects (repo_id);

-- Language byte counts of the default branch at `commit_sha`, computed by
-- the `repos.compute_languages` job. `languages` is an array of
-- [name, bytes] pairs ordered by bytes descending.
CREATE TABLE repo_languages (
    repo_id      BIGINT PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    commit_sha   TEXT NOT NULL,
    languages    JSONB NOT NULL DEFAULT '[]',
    computed_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Repository rulesets (subset of GitHub's rulesets API).
CREATE TABLE repo_rulesets (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id        BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    name           TEXT NOT NULL,
    target         TEXT NOT NULL DEFAULT 'branch' CHECK (target IN ('branch', 'tag')),
    enforcement    TEXT NOT NULL DEFAULT 'active' CHECK (enforcement IN ('disabled', 'active', 'evaluate')),
    -- {"ref_name": {"include": [..], "exclude": [..]}}
    conditions     JSONB NOT NULL DEFAULT '{}',
    -- [{"type": "deletion"}, {"type": "pull_request", "parameters": {..}}, ...]
    rules          JSONB NOT NULL DEFAULT '[]',
    -- [{"actor_id": 1, "actor_type": "RepositoryRole", "bypass_mode": "always"}]
    bypass_actors  JSONB NOT NULL DEFAULT '[]',
    created_by_id  BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX repo_rulesets_name_key ON repo_rulesets (repo_id, lower(name));

-- Autolink references (`JIRA-123` → URL).
CREATE TABLE repo_autolinks (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id          BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    key_prefix       TEXT NOT NULL,
    url_template     TEXT NOT NULL,
    is_alphanumeric  BOOLEAN NOT NULL DEFAULT true,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX repo_autolinks_prefix_key ON repo_autolinks (repo_id, lower(key_prefix));

-- List endpoints: /user/starred, /users/{u}/starred, /stargazers,
-- /subscribers, /user/subscriptions, /forks, collaborators, invitations.
CREATE INDEX stars_user_created_idx ON stars (user_id, created_at DESC, repo_id);
CREATE INDEX watches_user_idx ON watches (user_id, created_at) WHERE subscribed;
CREATE INDEX repositories_template_idx ON repositories (template_repository_id)
    WHERE template_repository_id IS NOT NULL;
CREATE INDEX repositories_parent_created_idx ON repositories (parent_id, created_at DESC)
    WHERE parent_id IS NOT NULL;
CREATE INDEX repo_invitations_repo_idx ON repo_invitations (repo_id, id);
CREATE INDEX deploy_keys_repo_idx ON deploy_keys (repo_id, id);
