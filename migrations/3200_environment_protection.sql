-- P20: environment protection rules and deployment approvals.

-- Protection settings of an environment (GitHub `PUT /environments/{env}`).
-- `branch_policy`: NULL = any branch or tag may deploy; 'protected' = only
-- protected branches; 'custom' = names matching a row of
-- actions_environment_branch_policies.
ALTER TABLE actions_environments
    ADD COLUMN wait_timer          INT NOT NULL DEFAULT 0 CHECK (wait_timer BETWEEN 0 AND 43200),
    ADD COLUMN prevent_self_review BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN can_admins_bypass   BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN branch_policy       TEXT CHECK (branch_policy IN ('protected', 'custom'));

-- Required reviewers (at most 6 per environment): users or teams.
CREATE TABLE actions_environment_reviewers (
    environment_id  BIGINT NOT NULL REFERENCES actions_environments (id) ON DELETE CASCADE,
    position        INT NOT NULL,
    user_id         BIGINT REFERENCES users (id) ON DELETE CASCADE,
    team_id         BIGINT REFERENCES teams (id) ON DELETE CASCADE,
    CHECK (num_nonnulls(user_id, team_id) = 1),
    PRIMARY KEY (environment_id, position)
);
CREATE INDEX actions_environment_reviewers_user_idx ON actions_environment_reviewers (user_id)
    WHERE user_id IS NOT NULL;
CREATE INDEX actions_environment_reviewers_team_idx ON actions_environment_reviewers (team_id)
    WHERE team_id IS NOT NULL;

-- Custom deployment branch and tag policies (fnmatch-style name patterns).
CREATE TABLE actions_environment_branch_policies (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    environment_id  BIGINT NOT NULL REFERENCES actions_environments (id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    type            TEXT NOT NULL DEFAULT 'branch' CHECK (type IN ('branch', 'tag')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX actions_environment_branch_policies_key
    ON actions_environment_branch_policies (environment_id, type, name);

-- A job waiting for an environment's protection rules (wait timer and/or
-- required reviewers). The job row has status 'waiting' until the gate is
-- released (→ 'queued') or rejected (→ completed/failure).
CREATE TABLE actions_job_gates (
    job_id          BIGINT PRIMARY KEY REFERENCES actions_jobs (id) ON DELETE CASCADE,
    run_id          BIGINT NOT NULL REFERENCES actions_runs (id) ON DELETE CASCADE,
    environment_id  BIGINT NOT NULL REFERENCES actions_environments (id) ON DELETE CASCADE,
    wait_timer      INT NOT NULL DEFAULT 0,
    wait_until      TIMESTAMPTZ,
    needs_review    BOOLEAN NOT NULL,
    -- NULL while pending; 'approved' | 'rejected' once reviewed.
    review_state    TEXT CHECK (review_state IN ('approved', 'rejected')),
    released_at     TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX actions_job_gates_run_idx ON actions_job_gates (run_id);
CREATE INDEX actions_job_gates_env_idx ON actions_job_gates (environment_id);
CREATE INDEX actions_job_gates_due_idx ON actions_job_gates (wait_until)
    WHERE released_at IS NULL;

-- Reviews of pending deployments (`GET /actions/runs/{id}/approvals`).
CREATE TABLE actions_deployment_reviews (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    run_id            BIGINT NOT NULL REFERENCES actions_runs (id) ON DELETE CASCADE,
    user_id           BIGINT REFERENCES users (id) ON DELETE SET NULL,
    state             TEXT NOT NULL CHECK (state IN ('approved', 'rejected')),
    comment           TEXT NOT NULL DEFAULT '',
    environment_ids   BIGINT[] NOT NULL,
    environment_names TEXT[] NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX actions_deployment_reviews_run_idx ON actions_deployment_reviews (run_id, id);
CREATE INDEX actions_deployment_reviews_user_idx ON actions_deployment_reviews (user_id);

-- Classic branch protection: "Require deployments to succeed before
-- merging" (GraphQL `requiredDeploymentEnvironments`).
ALTER TABLE branch_protections
    ADD COLUMN required_deployment_environments TEXT[] NOT NULL DEFAULT '{}';
