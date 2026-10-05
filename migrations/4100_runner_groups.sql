-- P29: runner OS/arch, runner groups (organization and site), JIT runners.

ALTER TABLE actions_runners ADD COLUMN arch TEXT NOT NULL DEFAULT 'X64';

-- Runner groups. `org_id` NULL = a site (enterprise) group. Each scope has
-- one default group (the site default is created here and has id 1).
CREATE TABLE actions_runner_groups (
    id                          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    org_id                      BIGINT REFERENCES users (id) ON DELETE CASCADE,
    name                        TEXT NOT NULL,
    -- Org groups: all | selected (repositories) | private (private repos).
    -- Site groups: all | selected (organizations).
    visibility                  TEXT NOT NULL DEFAULT 'all'
                                CHECK (visibility IN ('all', 'selected', 'private')),
    is_default                  BOOLEAN NOT NULL DEFAULT false,
    allows_public_repositories  BOOLEAN NOT NULL DEFAULT false,
    restricted_to_workflows     BOOLEAN NOT NULL DEFAULT false,
    -- `owner/repo/.github/workflows/x.yml@ref` (the `@ref` part is optional).
    selected_workflows          TEXT[] NOT NULL DEFAULT '{}',
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX actions_runner_groups_name_key
    ON actions_runner_groups (COALESCE(org_id, 0), lower(name));
CREATE UNIQUE INDEX actions_runner_groups_default_key
    ON actions_runner_groups (COALESCE(org_id, 0)) WHERE is_default;
CREATE INDEX actions_runner_groups_org_idx ON actions_runner_groups (org_id, id);

-- Selected repositories of an organization group.
CREATE TABLE actions_runner_group_repos (
    group_id  BIGINT NOT NULL REFERENCES actions_runner_groups (id) ON DELETE CASCADE,
    repo_id   BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, repo_id)
);
CREATE INDEX actions_runner_group_repos_repo_idx ON actions_runner_group_repos (repo_id);

-- Selected organizations of a site group.
CREATE TABLE actions_runner_group_orgs (
    group_id  BIGINT NOT NULL REFERENCES actions_runner_groups (id) ON DELETE CASCADE,
    org_id    BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, org_id)
);
CREATE INDEX actions_runner_group_orgs_org_idx ON actions_runner_group_orgs (org_id);

-- NULL: no group restrictions (repository runners, the built-in runner).
ALTER TABLE actions_runners
    ADD COLUMN runner_group_id BIGINT REFERENCES actions_runner_groups (id) ON DELETE SET NULL;
CREATE INDEX actions_runners_group_idx ON actions_runners (runner_group_id, id)
    WHERE runner_group_id IS NOT NULL;
CREATE INDEX actions_runners_site_idx ON actions_runners (id)
    WHERE repo_id IS NULL AND org_id IS NULL;

INSERT INTO actions_runner_groups (org_id, name, is_default, allows_public_repositories)
VALUES (NULL, 'Default', true, true);

INSERT INTO actions_runner_groups (org_id, name, is_default, allows_public_repositories)
SELECT DISTINCT org_id, 'Default', true, true FROM actions_runners WHERE org_id IS NOT NULL;

UPDATE actions_runners r SET runner_group_id = g.id
  FROM actions_runner_groups g
 WHERE g.is_default AND r.org_id IS NOT NULL AND g.org_id = r.org_id;

UPDATE actions_runners r SET runner_group_id = g.id
  FROM actions_runner_groups g
 WHERE g.is_default AND g.org_id IS NULL
   AND r.org_id IS NULL AND r.repo_id IS NULL AND NOT r.builtin;

-- Admin queue view (queued + in-progress jobs).
CREATE INDEX actions_jobs_active_idx ON actions_jobs (id)
    WHERE status IN ('queued', 'in_progress');
