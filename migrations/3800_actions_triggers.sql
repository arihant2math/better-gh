-- P26: job-level `concurrency` and the workflow status badge.

-- `jobs.<id>.concurrency.group` (evaluated); a job waits as `pending` while
-- another job of the group is queued or running.
ALTER TABLE actions_jobs ADD COLUMN concurrency_group TEXT;
CREATE INDEX actions_jobs_concurrency_idx ON actions_jobs (repo_id, concurrency_group, id)
    WHERE concurrency_group IS NOT NULL AND status <> 'completed';

-- badge.svg: latest completed run of a workflow on a branch.
CREATE INDEX actions_runs_badge_idx ON actions_runs (workflow_id, head_branch, id DESC)
    WHERE status = 'completed';
