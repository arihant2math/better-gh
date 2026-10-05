-- Actions: workflows, runs, jobs, runners, artifacts, secrets, variables,
-- environments. Check suites/runs live in the core tables (0004) and are
-- linked from runs/jobs.

CREATE TABLE actions_workflows (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id          BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    -- `.github/workflows/ci.yml`
    path             TEXT NOT NULL,
    name             TEXT NOT NULL,
    state            TEXT NOT NULL DEFAULT 'active' CHECK (state IN (
                         'active', 'deleted', 'disabled_fork', 'disabled_inactivity',
                         'disabled_manually')),
    next_run_number  BIGINT NOT NULL DEFAULT 1,
    -- Cron schedules of the default branch's version of the file.
    schedules        TEXT[] NOT NULL DEFAULT '{}',
    -- Scheduler high-water mark: crons fire for times after this.
    schedule_checked_at  TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX actions_workflows_path_key ON actions_workflows (repo_id, path);
CREATE INDEX actions_workflows_scheduled_idx ON actions_workflows (id)
    WHERE state = 'active' AND schedules <> '{}';

CREATE TABLE actions_runs (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id              BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    workflow_id          BIGINT NOT NULL REFERENCES actions_workflows (id) ON DELETE CASCADE,
    run_number           BIGINT NOT NULL,
    run_attempt          INTEGER NOT NULL DEFAULT 1,
    name                 TEXT NOT NULL,
    display_title        TEXT NOT NULL,
    event                TEXT NOT NULL,
    status               TEXT NOT NULL DEFAULT 'queued' CHECK (status IN (
                             'requested', 'queued', 'in_progress', 'completed', 'waiting',
                             'pending')),
    conclusion           TEXT CHECK (conclusion IN (
                             'success', 'failure', 'neutral', 'cancelled', 'skipped',
                             'timed_out', 'action_required', 'startup_failure', 'stale')),
    -- Full ref (`refs/heads/main`, `refs/pull/1/merge`, `refs/tags/v1`).
    ref                  TEXT NOT NULL,
    head_branch          TEXT,
    head_sha             TEXT NOT NULL,
    head_repo_id         BIGINT REFERENCES repositories (id) ON DELETE SET NULL,
    actor_id             BIGINT REFERENCES users (id) ON DELETE SET NULL,
    triggering_actor_id  BIGINT REFERENCES users (id) ON DELETE SET NULL,
    check_suite_id       BIGINT REFERENCES check_suites (id) ON DELETE SET NULL,
    -- Issue ids of associated pull requests.
    pull_request_ids     BIGINT[] NOT NULL DEFAULT '{}',
    -- `github.event` payload.
    event_payload        JSONB NOT NULL DEFAULT '{}',
    -- Snapshot of the workflow file at head_sha and its parsed form.
    workflow_yaml        TEXT NOT NULL,
    workflow_def         JSONB NOT NULL,
    inputs               JSONB,
    concurrency_group    TEXT,
    run_started_at       TIMESTAMPTZ,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX actions_runs_number_key ON actions_runs (workflow_id, run_number);
CREATE INDEX actions_runs_repo_idx ON actions_runs (repo_id, id DESC);
CREATE INDEX actions_runs_sha_idx ON actions_runs (repo_id, head_sha);
CREATE INDEX actions_runs_suite_idx ON actions_runs (check_suite_id);
CREATE INDEX actions_runs_active_idx ON actions_runs (repo_id, concurrency_group, id)
    WHERE status <> 'completed' AND concurrency_group IS NOT NULL;
CREATE INDEX actions_runs_actor_idx ON actions_runs (actor_id);

CREATE TABLE actions_runners (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Scope: repository runner, organization runner, or (both NULL) a
    -- site-wide runner such as the built-in one.
    repo_id       BIGINT REFERENCES repositories (id) ON DELETE CASCADE,
    org_id        BIGINT REFERENCES users (id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    os            TEXT NOT NULL DEFAULT 'Linux',
    -- Lower-cased labels: read-only system labels and custom labels.
    system_labels TEXT[] NOT NULL DEFAULT '{self-hosted,linux,x64}',
    labels        TEXT[] NOT NULL DEFAULT '{}',
    token_hash    TEXT NOT NULL UNIQUE,
    ephemeral     BOOLEAN NOT NULL DEFAULT false,
    builtin       BOOLEAN NOT NULL DEFAULT false,
    busy          BOOLEAN NOT NULL DEFAULT false,
    last_seen_at  TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (repo_id IS NULL OR org_id IS NULL)
);
CREATE INDEX actions_runners_repo_idx ON actions_runners (repo_id, id);
CREATE INDEX actions_runners_org_idx ON actions_runners (org_id, id);

-- Short-lived runner registration / removal tokens.
CREATE TABLE actions_runner_tokens (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('registration', 'remove')),
    token_hash  TEXT NOT NULL UNIQUE,
    repo_id     BIGINT REFERENCES repositories (id) ON DELETE CASCADE,
    org_id      BIGINT REFERENCES users (id) ON DELETE CASCADE,
    expires_at  TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX actions_runner_tokens_expires_idx ON actions_runner_tokens (expires_at);

CREATE TABLE actions_jobs (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    run_id             BIGINT NOT NULL REFERENCES actions_runs (id) ON DELETE CASCADE,
    repo_id            BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    run_attempt        INTEGER NOT NULL DEFAULT 1,
    -- Job id in the workflow file.
    job_key            TEXT NOT NULL,
    name               TEXT NOT NULL,
    matrix             JSONB,
    status             TEXT NOT NULL DEFAULT 'queued' CHECK (status IN (
                           'queued', 'in_progress', 'completed', 'waiting', 'pending')),
    conclusion         TEXT CHECK (conclusion IN (
                           'success', 'failure', 'neutral', 'cancelled', 'skipped',
                           'timed_out', 'action_required')),
    head_sha           TEXT NOT NULL,
    head_branch        TEXT,
    -- Lower-cased `runs-on` labels the runner must carry.
    labels             TEXT[] NOT NULL DEFAULT '{}',
    runner_id          BIGINT REFERENCES actions_runners (id) ON DELETE SET NULL,
    runner_name        TEXT,
    check_run_id       BIGINT REFERENCES check_runs (id) ON DELETE SET NULL,
    -- Resolved definition handed to the runner (see runner::JobSpec).
    spec               JSONB,
    -- [{number, name, status, conclusion, started_at, completed_at}]
    steps              JSONB NOT NULL DEFAULT '[]',
    outputs            JSONB NOT NULL DEFAULT '{}',
    continue_on_error  BOOLEAN NOT NULL DEFAULT false,
    timeout_minutes    INTEGER NOT NULL DEFAULT 360,
    cancel_requested   BOOLEAN NOT NULL DEFAULT false,
    -- access_tokens row backing GITHUB_TOKEN while the job runs.
    token_id           BIGINT,
    -- Logs of a job copied into a re-run attempt live under this job.
    logs_job_id        BIGINT,
    started_at         TIMESTAMPTZ,
    completed_at       TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX actions_jobs_run_idx ON actions_jobs (run_id, run_attempt, id);
CREATE INDEX actions_jobs_queued_idx ON actions_jobs (id) WHERE status = 'queued';
CREATE INDEX actions_jobs_repo_idx ON actions_jobs (repo_id);
CREATE INDEX actions_jobs_runner_idx ON actions_jobs (runner_id) WHERE runner_id IS NOT NULL;
CREATE INDEX actions_jobs_check_run_idx ON actions_jobs (check_run_id);

CREATE TABLE actions_artifacts (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id        BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    run_id         BIGINT NOT NULL REFERENCES actions_runs (id) ON DELETE CASCADE,
    job_id         BIGINT REFERENCES actions_jobs (id) ON DELETE SET NULL,
    name           TEXT NOT NULL,
    size_in_bytes  BIGINT NOT NULL DEFAULT 0,
    -- `sha256:<hex>` of the stored zip.
    digest         TEXT,
    expired        BOOLEAN NOT NULL DEFAULT false,
    expires_at     TIMESTAMPTZ NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX actions_artifacts_run_name_key ON actions_artifacts (run_id, name);
CREATE INDEX actions_artifacts_repo_idx ON actions_artifacts (repo_id, id DESC);
CREATE INDEX actions_artifacts_expires_idx ON actions_artifacts (expires_at) WHERE NOT expired;

CREATE TABLE actions_environments (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX actions_environments_name_key ON actions_environments (repo_id, lower(name));

-- libsodium sealed-box key pairs for the GitHub `public-key` flow. The
-- private key is encrypted with the server key (see crypto.rs).
CREATE TABLE actions_keys (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id         BIGINT REFERENCES repositories (id) ON DELETE CASCADE,
    org_id          BIGINT REFERENCES users (id) ON DELETE CASCADE,
    key_id          TEXT NOT NULL,
    public_key      TEXT NOT NULL,
    secret_key_enc  BYTEA NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((repo_id IS NULL) <> (org_id IS NULL))
);
CREATE UNIQUE INDEX actions_keys_repo_key ON actions_keys (repo_id) WHERE repo_id IS NOT NULL;
CREATE UNIQUE INDEX actions_keys_org_key ON actions_keys (org_id) WHERE org_id IS NOT NULL;

-- Secrets (value encrypted with the server key) and variables (plain).
-- Exactly one of repo_id / org_id / environment_id is set.
CREATE TABLE actions_secrets (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id         BIGINT REFERENCES repositories (id) ON DELETE CASCADE,
    org_id          BIGINT REFERENCES users (id) ON DELETE CASCADE,
    environment_id  BIGINT REFERENCES actions_environments (id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    value_enc       BYTEA NOT NULL,
    -- Organization secrets: all | private | selected
    visibility      TEXT CHECK (visibility IN ('all', 'private', 'selected')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(repo_id, org_id, environment_id) = 1)
);
CREATE UNIQUE INDEX actions_secrets_repo_key ON actions_secrets (repo_id, upper(name)) WHERE repo_id IS NOT NULL;
CREATE UNIQUE INDEX actions_secrets_org_key ON actions_secrets (org_id, upper(name)) WHERE org_id IS NOT NULL;
CREATE UNIQUE INDEX actions_secrets_env_key ON actions_secrets (environment_id, upper(name)) WHERE environment_id IS NOT NULL;

CREATE TABLE actions_secret_repos (
    secret_id  BIGINT NOT NULL REFERENCES actions_secrets (id) ON DELETE CASCADE,
    repo_id    BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    PRIMARY KEY (secret_id, repo_id)
);
CREATE INDEX actions_secret_repos_repo_idx ON actions_secret_repos (repo_id);

CREATE TABLE actions_variables (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id         BIGINT REFERENCES repositories (id) ON DELETE CASCADE,
    org_id          BIGINT REFERENCES users (id) ON DELETE CASCADE,
    environment_id  BIGINT REFERENCES actions_environments (id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    value           TEXT NOT NULL,
    visibility      TEXT CHECK (visibility IN ('all', 'private', 'selected')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(repo_id, org_id, environment_id) = 1)
);
CREATE UNIQUE INDEX actions_variables_repo_key ON actions_variables (repo_id, upper(name)) WHERE repo_id IS NOT NULL;
CREATE UNIQUE INDEX actions_variables_org_key ON actions_variables (org_id, upper(name)) WHERE org_id IS NOT NULL;
CREATE UNIQUE INDEX actions_variables_env_key ON actions_variables (environment_id, upper(name)) WHERE environment_id IS NOT NULL;

CREATE TABLE actions_variable_repos (
    variable_id  BIGINT NOT NULL REFERENCES actions_variables (id) ON DELETE CASCADE,
    repo_id      BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    PRIMARY KEY (variable_id, repo_id)
);
CREATE INDEX actions_variable_repos_repo_idx ON actions_variable_repos (repo_id);
