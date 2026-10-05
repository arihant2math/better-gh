import { describe, expect, it } from 'vitest';
import type { RunGraph, RunGraphCall, WorkflowJob } from '../../api/actions';
import { calledLabel, pendingCalled, rootKey } from './calls';

const job = (id: number, name: string) => ({ id, name, status: 'completed', conclusion: 'success' }) as WorkflowJob;

const call = (key: string, name: string, jobs: string[]): RunGraphCall => ({
  id: 1,
  key,
  root: rootKey(key),
  prefix: `${key}/`,
  name,
  uses: './.github/workflows/build.yml',
  workflow_ref: 'o/r/.github/workflows/build.yml@refs/heads/main',
  status: 'in_progress',
  conclusion: null,
  jobs: jobs.map((k) => ({ key: `${key}/${k}`, name: k, needs: [], matrix: false, uses: null })),
});

describe('reusable workflow calls', () => {
  it('maps called job keys to their top-level caller', () => {
    expect(rootKey('build')).toBe('build');
    expect(rootKey('build/test')).toBe('build');
    expect(rootKey('build.1/test/inner')).toBe('build');
  });

  it('strips the caller from called job names', () => {
    expect(calledLabel('Deploy / test', 'Deploy')).toBe('test');
    expect(calledLabel('Deploy (linux) / test', 'Deploy')).toBe('(linux) / test');
    expect(calledLabel('other', 'Deploy')).toBe('other');
  });

  it('lists called jobs that have no job row yet', () => {
    const graph: RunGraph = {
      run_id: 1,
      workflow_name: 'CI',
      jobs: [{ key: 'build', name: 'Build', needs: [], matrix: false, uses: './.github/workflows/build.yml' }],
      job_keys: { '10': 'build/lint', '11': 'build/nested/x' },
      calls: [call('build', 'Build', ['lint', 'test', 'nested']), call('other', 'Other', ['a'])],
    };
    const jobs = [job(10, 'Build / lint'), job(11, 'Build / nested / x')];
    expect(pendingCalled(graph, 'build', jobs)).toEqual(['Build / test']);
    expect(pendingCalled(graph, 'build', [])).toEqual(['Build / lint', 'Build / test', 'Build / nested']);
    expect(pendingCalled({ ...graph, calls: undefined }, 'build', jobs)).toEqual([]);
  });
});
