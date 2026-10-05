import { afterEach, describe, expect, it } from 'vitest';
import type { WorkflowJob, WorkflowRun } from '../api/actions';
import { actionsMock } from './actions';
import { MockServer } from './server';

const NOW = Date.parse('2026-10-05T12:00:00Z');
const BASE = '/api/v3/repos/acme/api';

let server: MockServer | null = null;
function mk(): MockServer {
  server = new MockServer(null, { now: NOW });
  return server;
}
afterEach(() => {
  server?.dispose();
  server = null;
});

async function get<T>(s: MockServer, path: string): Promise<{ status: number; body: T; headers: Headers }> {
  const res = await s.fetch(path);
  return { status: res.status, body: (await res.json()) as T, headers: res.headers };
}

async function post<T>(s: MockServer, path: string, body: unknown): Promise<{ status: number; body: T }> {
  const res = await s.fetch(path, { method: 'POST', body: JSON.stringify(body) });
  return { status: res.status, body: (res.status === 204 ? null : await res.json()) as T };
}

async function readAll(res: Response): Promise<{ event: string; data: unknown }[]> {
  const text = await res.text();
  return text
    .split('\n\n')
    .filter(Boolean)
    .map((f) => {
      const [ev, data] = f.split('\n');
      return { event: ev!.replace('event: ', ''), data: JSON.parse(data!.replace('data: ', '')) as unknown };
    });
}

type Runs = { total_count: number; workflow_runs: WorkflowRun[] };

describe('mock actions', () => {
  it('lists runs newest first with filters and pagination', async () => {
    const s = mk();
    const all = await get<Runs>(s, `${BASE}/actions/runs?per_page=100`);
    expect(all.status).toBe(200);
    expect(all.body.total_count).toBeGreaterThanOrEqual(150);
    const times = all.body.workflow_runs.map((r) => Date.parse(r.created_at));
    expect([...times].sort((a, b) => b - a)).toEqual(times);
    expect(all.body.workflow_runs.some((r) => r.status === 'in_progress')).toBe(true);
    expect(all.body.workflow_runs.some((r) => r.status === 'queued')).toBe(true);
    const r0 = all.body.workflow_runs[0]!;
    expect(r0.html_url).toBe(`/acme/api/actions/runs/${r0.id}`);
    expect(r0.actor?.login).toBeTruthy();
    expect(r0.head_commit?.id).toBe(r0.head_sha);

    const p2 = await get<Runs>(s, `${BASE}/actions/runs?per_page=10&page=2`);
    expect(p2.body.workflow_runs).toHaveLength(10);
    expect(p2.body.workflow_runs[0]!.id).toBe(all.body.workflow_runs[10]!.id);
    expect(p2.headers.get('Link')).toContain('rel="next"');

    const failed = await get<Runs>(s, `${BASE}/actions/runs?status=failure&per_page=100`);
    expect(failed.body.total_count).toBeGreaterThan(0);
    expect(failed.body.workflow_runs.every((r) => r.conclusion === 'failure')).toBe(true);
    const sched = await get<Runs>(s, `${BASE}/actions/runs?event=schedule&branch=main&per_page=100`);
    expect(sched.body.workflow_runs.every((r) => r.event === 'schedule' && r.head_branch === 'main')).toBe(true);
    const login = r0.actor!.login;
    const mine = await get<Runs>(s, `${BASE}/actions/runs?actor=${login}&per_page=100`);
    expect(mine.body.total_count).toBeGreaterThan(0);

    const byWf = await get<Runs>(s, `${BASE}/actions/workflows/nightly.yml/runs?per_page=100`);
    expect(byWf.body.workflow_runs.every((r) => r.path === '.github/workflows/nightly.yml')).toBe(true);
    const wfs = await get<{ total_count: number; workflows: { state: string }[] }>(s, `${BASE}/actions/workflows`);
    expect(wfs.body.total_count).toBe(4);
    expect(wfs.body.workflows.some((w) => w.state === 'disabled_manually')).toBe(true);
  });

  it('validates workflow_dispatch inputs', async () => {
    const s = mk();
    const url = `${BASE}/actions/workflows/ci.yml/dispatches`;
    expect((await post<{ message: string }>(s, url, { ref: 'main', inputs: {} })).body.message).toBe("Required input 'reason' not provided");
    expect((await post<{ message: string }>(s, url, { ref: 'main', inputs: { reason: 'x', log_level: 'loud' } })).body.message).toBe(
      "Provided value 'loud' for input 'log_level' not in the list of allowed values",
    );
    expect((await post<{ message: string }>(s, url, { ref: 'main', inputs: { reason: 'x', nope: '1' } })).body.message).toBe('Unexpected inputs provided: ["nope"]');
    const badRef = await post<{ message: string }>(s, url, { ref: 'no-such-branch', inputs: { reason: 'x' } });
    expect(badRef).toEqual({ status: 422, body: { message: 'No ref found for: no-such-branch' } });
    expect((await post<{ message: string }>(s, `${BASE}/actions/workflows/stale.yml/dispatches`, { ref: 'main' })).status).toBe(422);

    const form = await get<{ dispatchable: boolean; inputs: { name: string; type: string }[] }>(s, `/_bgh/actions/repos/acme/api/workflows/ci.yml/dispatch?ref=main`);
    expect(form.body.dispatchable).toBe(true);
    expect(form.body.inputs.map((i) => i.type)).toEqual(['string', 'choice', 'boolean', 'environment']);
    const noRef = await get<{ dispatchable: boolean; error: string }>(s, `/_bgh/actions/repos/acme/api/workflows/ci.yml/dispatch?ref=nope`);
    expect(noRef.body).toMatchObject({ dispatchable: false, error: 'No ref found for: nope' });

    const ok = await post<{ workflow_run_id: number; html_url: string }>(s, url, { ref: 'main', inputs: { reason: 'smoke', debug: 'true' }, return_run_details: true });
    expect(ok.status).toBe(200);
    const run = await get<WorkflowRun>(s, `${BASE}/actions/runs/${ok.body.workflow_run_id}`);
    expect(run.body).toMatchObject({ event: 'workflow_dispatch', status: 'queued', head_branch: 'main' });
  });

  it('streams logs of a completed job and serves the 100k-line log', async () => {
    const s = mk();
    const runs = await get<Runs>(s, `${BASE}/actions/workflows/ci.yml/runs?status=completed&per_page=1`);
    const run = runs.body.workflow_runs[0]!;
    const jobs = await get<{ jobs: WorkflowJob[] }>(s, `${BASE}/actions/runs/${run.id}/jobs`);
    const lint = jobs.body.jobs.find((j) => j.name === 'lint')!;
    expect(jobs.body.jobs.map((j) => j.name)).toContain('build (ubuntu)');
    expect(lint.check_run_url).toMatch(/\/check-runs\/\d+$/);

    const res = await s.fetch(`/_bgh/actions/jobs/${lint.id}/logs/stream`);
    expect(res.headers.get('content-type')).toBe('text/event-stream');
    const frames = await readAll(res);
    expect(frames.at(-1)).toEqual({ event: 'done', data: {} });
    const logs = frames.filter((f) => f.event === 'log') as { data: { step: number; text: string } }[];
    expect(logs.length).toBeGreaterThan(3);
    expect(logs[0]!.data.step).toBe(1);
    expect(logs[0]!.data.text).toMatch(/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{7}Z /);
    expect(logs.some((f) => f.data.text.includes('##[group]'))).toBe(true);

    const test = jobs.body.jobs.find((j) => j.name === 'test')!;
    const full = await s.fetch(`${BASE}/actions/jobs/${test.id}/logs`);
    const text = await full.text();
    expect(text.split('\n').length).toBeGreaterThan(100_000);
    const graph = await get<{ jobs: { key: string; needs: string[] }[]; job_keys: Record<string, string> }>(s, `/_bgh/actions/repos/acme/api/runs/${run.id}/graph`);
    expect(graph.body.jobs.find((j) => j.key === 'build')?.needs).toEqual(['lint']);
    expect(graph.body.job_keys[String(test.id)]).toBe('test');
  });

  it('simulates a dispatched run and emits workflow_run / workflow_job deltas', async () => {
    const s = mk();
    const ok = await post<{ workflow_run_id: number }>(s, `${BASE}/actions/workflows/deploy.yml/dispatches`, { ref: 'main', inputs: {}, return_run_details: true });
    const runId = ok.body.workflow_run_id;
    const mock = actionsMock(s)!;
    let t = Date.now();
    let streamed: Promise<{ event: string; data: unknown }[]> | null = null;
    for (let i = 0; i < 400 && mock.activeRuns().includes(runId); i++) {
      mock.tick((t += 1000));
      if (!streamed) {
        const jobs = await get<{ jobs: WorkflowJob[] }>(s, `${BASE}/actions/runs/${runId}/jobs`);
        const first = jobs.body.jobs[0];
        if (first?.status === 'in_progress') streamed = s.fetch(`/_bgh/actions/jobs/${first.id}/logs/stream`).then(readAll);
      }
      await Promise.resolve();
    }
    const run = await get<WorkflowRun>(s, `${BASE}/actions/runs/${runId}`);
    expect(run.body.status).toBe('completed');
    const deltas = s.log.filter((d) => (d.model as string).startsWith('workflow_'));
    const runDeltas = deltas.filter((d) => (d.model as string) === 'workflow_run' && d.mid === runId);
    expect(runDeltas[0]!.a).toBe('I');
    expect(runDeltas.at(-1)!.d).toMatchObject({ id: runId, status: 'completed', run_attempt: 1, workflow_id: expect.any(Number) });
    const repoId = (runDeltas[0]!.d as { repo_id: number }).repo_id;
    expect(runDeltas.every((d) => d.scope === `repo:${repoId}`)).toBe(true);
    const jobDeltas = deltas.filter((d) => (d.model as string) === 'workflow_job');
    expect(jobDeltas.some((d) => d.a === 'I')).toBe(true);
    expect(jobDeltas.some((d) => d.a === 'U' && Array.isArray((d.d as { steps: unknown[] }).steps))).toBe(true);
    expect(jobDeltas.at(-1)!.d).toMatchObject({ run_id: runId, status: 'completed' });

    const frames = await streamed!;
    expect(frames.at(-1)!.event).toBe('done');
    expect(frames.filter((f) => f.event === 'log').length).toBeGreaterThan(3);

    // Re-run → new attempt.
    const rr = await post(s, `${BASE}/actions/runs/${runId}/rerun`, {});
    expect(rr.status).toBe(201);
    const again = await get<WorkflowRun>(s, `${BASE}/actions/runs/${runId}`);
    expect(again.body).toMatchObject({ run_attempt: 2, status: 'queued' });
    const cancel = await post(s, `${BASE}/actions/runs/${runId}/cancel`, {});
    expect(cancel.status).toBe(202);
    expect((await get<WorkflowRun>(s, `${BASE}/actions/runs/${runId}`)).body.conclusion).toBe('cancelled');
    expect((await get<WorkflowRun>(s, `${BASE}/actions/runs/${runId}/attempts/1`)).body.run_attempt).toBe(1);
  });

  it('serves settings: secrets, variables, environments, runners', async () => {
    const s = mk();
    const key = await get<{ key_id: string; key: string }>(s, `${BASE}/actions/secrets/public-key`);
    expect(atob(key.body.key)).toHaveLength(32);
    const put = (name: string) => s.fetch(`${BASE}/actions/secrets/${name}`, { method: 'PUT', body: JSON.stringify({ encrypted_value: 'c2VjcmV0', key_id: key.body.key_id }) });
    expect((await put('NEW_SECRET')).status).toBe(201);
    expect((await put('NEW_SECRET')).status).toBe(204);
    const secrets = await get<{ total_count: number; secrets: { name: string }[] }>(s, `${BASE}/actions/secrets`);
    expect(secrets.body.secrets.map((x) => x.name)).toContain('NEW_SECRET');
    expect((await post(s, `${BASE}/actions/variables`, { name: 'foo', value: 'bar' })).status).toBe(201);
    expect((await post(s, `${BASE}/actions/variables`, { name: 'FOO', value: 'bar' })).status).toBe(409);
    const envs = await get<{ total_count: number }>(s, `${BASE}/environments`);
    expect(envs.body.total_count).toBe(2);
    const envSecrets = await get<{ total_count: number }>(s, `${BASE}/environments/production/secrets`);
    expect(envSecrets.body.total_count).toBe(2);
    const org = await get<{ secrets: { visibility: string }[] }>(s, '/api/v3/orgs/acme/actions/secrets');
    expect(org.body.secrets.map((x) => x.visibility).sort()).toEqual(['all', 'private', 'selected']);
    const runners = await get<{ runners: { id: number; status: string; busy: boolean; labels: { type: string }[] }[] }>(s, `${BASE}/actions/runners`);
    expect(runners.body.runners.some((r) => r.status === 'offline')).toBe(true);
    expect(runners.body.runners.some((r) => r.busy)).toBe(true);
    const tok = await post<{ token: string }>(s, `${BASE}/actions/runners/registration-token`, {});
    expect(tok.status).toBe(201);
    const id = runners.body.runners[0]!.id;
    const labels = await post<{ labels: { name: string; type: string }[] }>(s, `${BASE}/actions/runners/${id}/labels`, { labels: ['fast'] });
    expect(labels.body.labels.find((l) => l.name === 'fast')?.type).toBe('custom');
    expect(labels.body.labels.find((l) => l.name === 'self-hosted')?.type).toBe('read-only');
  });
});
