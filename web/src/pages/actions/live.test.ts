import { describe, expect, it } from 'vitest';
import type { WorkflowJob, WorkflowRun } from '../../api/actions';
import type { Delta } from '../../sync/protocol';
import { applyActionDeltas, jobs, mergeJobs, mergeRuns, runs } from './live';

const run = (id: number, patch: Partial<WorkflowRun> = {}): WorkflowRun =>
  ({
    id,
    name: 'CI',
    head_branch: 'main',
    head_sha: 'abc',
    path: '.github/workflows/ci.yml',
    display_title: 'CI',
    run_number: id,
    event: 'push',
    status: 'queued',
    conclusion: null,
    workflow_id: 1,
    check_suite_id: null,
    html_url: '',
    pull_requests: [],
    created_at: '2026-10-05T08:00:00Z',
    updated_at: '2026-10-05T08:00:00Z',
    actor: null,
    triggering_actor: null,
    run_attempt: 1,
    run_started_at: '2026-10-05T08:00:00Z',
    head_commit: null,
    ...patch,
  });

const delta = (model: string, mid: number, d: Record<string, unknown>, a: Delta['a'] = 'U'): Delta =>
  ({ id: mid, scope: 'repo:7', model, mid, a, d }) as unknown as Delta;

describe('actions live store', () => {
  it('patches known runs and jobs from deltas', () => {
    mergeRuns([run(1)]);
    mergeJobs([{ id: 10, run_id: 1, name: 'build', status: 'queued', conclusion: null, steps: [] } as unknown as WorkflowJob]);
    applyActionDeltas([
      delta('workflow_run', 1, { repo_id: 7, status: 'completed', conclusion: 'success', updated_at: '2026-10-05T08:05:00Z', run_attempt: 1 }),
      delta('workflow_job', 10, { run_id: 1, status: 'completed', conclusion: 'failure', steps: [{ number: 1, name: 'Set up job', status: 'completed', conclusion: 'success' }] }),
    ]);
    expect(runs.get(1)).toMatchObject({ status: 'completed', conclusion: 'success', updated_at: '2026-10-05T08:05:00Z' });
    expect(jobs.get(10)).toMatchObject({ status: 'completed', conclusion: 'failure' });
    expect(jobs.get(10)!.steps).toHaveLength(1);
  });

  it('keeps a newer live run over an older REST snapshot', () => {
    mergeRuns([run(2, { status: 'completed', updated_at: '2026-10-05T09:00:00Z' })]);
    mergeRuns([run(2, { status: 'in_progress', updated_at: '2026-10-05T08:30:00Z' })]);
    expect(runs.get(2)!.status).toBe('completed');
  });

  it('ignores unknown rows and other models', () => {
    applyActionDeltas([delta('workflow_run', 99, { repo_id: 7, status: 'queued' }, 'I'), delta('issue', 5, { title: 'x' })]);
    expect(runs.has(99)).toBe(false);
  });
});
