-- P19: Deployments API (`/repos/{o}/{r}/deployments`) and deployment
-- statuses. Environments are the existing `actions_environments` rows
-- (auto-created on first deployment); P20 adds protection rules to them.

CREATE TABLE deployments (
    id                      BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id                 BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    -- Current environment (a status may move the deployment to another one).
    environment_id          BIGINT REFERENCES actions_environments (id) ON DELETE SET NULL,
    environment             TEXT NOT NULL,
    original_environment    TEXT NOT NULL,
    sha                     TEXT NOT NULL,
    ref                     TEXT NOT NULL,
    task                    TEXT NOT NULL DEFAULT 'deploy',
    -- Object or string, as given by the client.
    payload                 JSONB NOT NULL DEFAULT '{}',
    description             TEXT,
    creator_id              BIGINT REFERENCES users (id) ON DELETE SET NULL,
    transient_environment   BOOLEAN NOT NULL DEFAULT false,
    production_environment  BOOLEAN NOT NULL DEFAULT false,
    -- Latest status (denormalized for lists and the delete rule); NULL
    -- until the first status is posted.
    state                   TEXT CHECK (state IN ('error', 'failure', 'inactive', 'in_progress',
                                                  'queued', 'pending', 'success')),
    latest_status_id        BIGINT,
    -- Actions job that created the deployment (P20: `environment:` jobs).
    run_id                  BIGINT REFERENCES actions_runs (id) ON DELETE SET NULL,
    job_id                  BIGINT REFERENCES actions_jobs (id) ON DELETE SET NULL,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX deployments_repo_idx ON deployments (repo_id, id DESC);
CREATE INDEX deployments_env_idx ON deployments (repo_id, lower(environment), id DESC);
CREATE INDEX deployments_sha_idx ON deployments (repo_id, sha);
CREATE INDEX deployments_environment_id_idx ON deployments (environment_id);
CREATE INDEX deployments_creator_idx ON deployments (creator_id);
CREATE INDEX deployments_run_idx ON deployments (run_id) WHERE run_id IS NOT NULL;
CREATE INDEX deployments_job_idx ON deployments (job_id) WHERE job_id IS NOT NULL;

CREATE TABLE deployment_statuses (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    deployment_id    BIGINT NOT NULL REFERENCES deployments (id) ON DELETE CASCADE,
    repo_id          BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    state            TEXT NOT NULL CHECK (state IN ('error', 'failure', 'inactive', 'in_progress',
                                                    'queued', 'pending', 'success')),
    description      TEXT NOT NULL DEFAULT '',
    environment      TEXT NOT NULL,
    target_url       TEXT NOT NULL DEFAULT '',
    log_url          TEXT NOT NULL DEFAULT '',
    environment_url  TEXT NOT NULL DEFAULT '',
    creator_id       BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX deployment_statuses_deployment_idx ON deployment_statuses (deployment_id, id DESC);
CREATE INDEX deployment_statuses_repo_idx ON deployment_statuses (repo_id);
CREATE INDEX deployment_statuses_creator_idx ON deployment_statuses (creator_id);
