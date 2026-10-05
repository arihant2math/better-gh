-- P16: reusable workflows (`jobs.<id>.uses` / `on.workflow_call`).

-- `call` rows stand for a job that calls a reusable workflow (one per
-- matrix combination). They are never claimed by runners and have no check
-- run or logs; the called workflow's jobs are ordinary `job` rows whose
-- `job_key` is prefixed with the call's key (`build/test`, `build.1/test`).
-- A call completes when its jobs did, with the called workflow's outputs.
-- (`actions_jobs.concurrency_group`, used for the calling job's
-- `concurrency:`, comes from P26's migration 3800.)
ALTER TABLE actions_jobs
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'job' CHECK (kind IN ('job', 'call'));

-- Who may call this (private or internal) repository's reusable workflows
-- and actions (GitHub's `/actions/permissions/access`). No row = `none`.
CREATE TABLE actions_repo_access (
    repo_id      BIGINT PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    access_level TEXT NOT NULL CHECK (access_level IN ('none', 'user', 'organization',
                                                       'enterprise')),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
